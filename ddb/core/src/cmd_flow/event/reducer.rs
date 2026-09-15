//! Applies decoded debugger events to the runtime model.
//!
//! The reducer is the single write path for thread and group lifecycle state.
//! Applying an event yields an [`EventEffect`](render::EventEffect) capturing
//! every translated identity; presentation is delegated to the pure
//! [`render`](render::render) step so state transitions and wire output stay
//! independently testable.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tracing::trace;

use crate::{
    cmd_flow::breakpoint::BreakpointEventPublisher,
    debugger::protocol::StreamKind,
    state::{GlobalThreadId, RuntimeModel, ThreadStatus, ThreadStopKind, ThreadStopReason},
};

use super::{
    render::{self, EventEffect, StoppedThreadField},
    DebuggerEvent, DebuggerEventKind, EventProjection, ThreadSet,
};

pub(crate) struct DebuggerEventReducer {
    model: Arc<RuntimeModel>,
    breakpoint_events: Arc<BreakpointEventPublisher>,
}

impl std::fmt::Debug for DebuggerEventReducer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DebuggerEventReducer").finish()
    }
}

impl DebuggerEventReducer {
    pub(crate) fn new(
        model: Arc<RuntimeModel>,
        breakpoint_events: Arc<BreakpointEventPublisher>,
    ) -> Arc<Self> {
        Arc::new(Self {
            model,
            breakpoint_events,
        })
    }

    pub(crate) async fn project(&self, event: DebuggerEvent, sid: u64) -> Result<EventProjection> {
        let effect = self.apply(&event, sid).await?;
        let projection = render::render(effect, event, sid);
        if let Some(output) = projection.output.as_ref() {
            self.breakpoint_events
                .broadcast_debugger_output(sid, output)
                .await;
        }
        Ok(projection)
    }

    pub(crate) async fn project_stream(
        &self,
        sid: u64,
        kind: StreamKind,
        message: String,
    ) -> EventProjection {
        let projection = EventProjection::debugger_stream(kind, message);
        if let Some(output) = projection.output.as_ref() {
            self.breakpoint_events
                .broadcast_debugger_output(sid, output)
                .await;
        }
        projection
    }

    /// Applies the event's state transitions and captures the identities its
    /// rendered output will refer to.
    async fn apply(&self, event: &DebuggerEvent, sid: u64) -> Result<EventEffect> {
        match &event.kind {
            DebuggerEventKind::BreakpointModified => Ok(EventEffect::Ignored),
            DebuggerEventKind::BreakpointDeleted {
                local_breakpoint_id,
            } => {
                let change = self
                    .model
                    .record_local_breakpoint_deletion(sid, *local_breakpoint_id);
                self.breakpoint_events.publish_state_change(change).await;
                Ok(EventEffect::Ignored)
            }
            DebuggerEventKind::ThreadCreated {
                local_thread_id,
                local_group_id,
            } => {
                let identity = self
                    .model
                    .register_thread(sid, *local_thread_id, local_group_id)
                    .await?;
                let alias = self
                    .model
                    .session_service_identity(sid)
                    .await
                    .map(|identity| identity.alias)
                    .unwrap_or_else(|| "UNKNOWN".to_string());
                let group_hash = self
                    .model
                    .group_hash_by_session(sid)
                    .unwrap_or_else(|| "UNKNOWN".to_string());
                Ok(EventEffect::ThreadCreated {
                    identity,
                    alias,
                    group_hash,
                })
            }
            DebuggerEventKind::ThreadExited {
                local_thread_id,
                local_group_id,
            } => {
                let identity = self
                    .model
                    .remove_thread(sid, *local_thread_id, local_group_id)
                    .await?;
                Ok(EventEffect::ThreadExited { identity })
            }
            DebuggerEventKind::Running { threads } => {
                self.update_thread_statuses(sid, threads, ThreadStatus::RUNNING, None)
                    .await?;
                Ok(EventEffect::Running {
                    global_threads: self.global_threads(sid, threads)?,
                })
            }
            DebuggerEventKind::Stopped {
                reasons,
                signal_name,
                thread,
                stopped_threads,
                local_breakpoint_id,
                location,
            } => {
                let is_exit = reasons.iter().any(|reason| reason.contains("exit"));
                let is_breakpoint = reasons.iter().any(|reason| reason == "breakpoint-hit");
                if is_exit {
                    return Ok(EventEffect::Exited {
                        reasons: reasons.clone(),
                    });
                }

                let Some(thread) = thread else {
                    return Ok(EventEffect::Passthrough);
                };
                if is_breakpoint {
                    if let ThreadSet::One(local_thread_id) = thread {
                        self.model
                            .select_local_thread(sid, *local_thread_id)
                            .await?;
                    }
                }

                let breakpoint = if is_breakpoint {
                    match local_breakpoint_id.and_then(|local_breakpoint_id| {
                        self.model.record_breakpoint_hit(sid, local_breakpoint_id)
                    }) {
                        Some((breakpoint_id, sub_breakpoint_id, breakpoint)) => {
                            self.breakpoint_events.publish_update(breakpoint).await;
                            Some((breakpoint_id, sub_breakpoint_id))
                        }
                        None => None,
                    }
                } else {
                    None
                };

                // Commit the stopped set and principal thread's reason together. A
                // topology notification must never expose a new stop without its details.
                let has_stopped_threads = stopped_threads.is_some();
                let stopped_threads = stopped_threads.as_ref().unwrap_or(thread);
                let mut stopped_ids = self.global_threads(sid, stopped_threads)?;
                stopped_ids.extend(self.global_threads(sid, thread)?);
                stopped_ids.sort_unstable_by_key(|id| id.value());
                stopped_ids.dedup();
                let local_ids = stopped_ids
                    .iter()
                    .map(|id| {
                        self.model
                            .local_thread_id(*id)
                            .map(|id| id.1)
                            .ok_or_else(|| anyhow!("unknown stopped thread {id}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let principal = match thread {
                    ThreadSet::One(id) => Some(*id),
                    _ => None,
                };
                self.model
                    .update_thread_statuses_with_details(
                        sid,
                        &local_ids,
                        ThreadStatus::STOPPED,
                        principal.zip(location.clone()),
                        Some(ThreadStopReason {
                            kind: stop_kind(reasons),
                            signal_name: (stop_kind(reasons) == ThreadStopKind::Signal)
                                .then(|| signal_name.clone())
                                .flatten(),
                            breakpoint_id: breakpoint.map(|(id, _)| id),
                            thread_id: principal
                                .and_then(|id| self.model.global_thread_id(sid, id))
                                .map(|id| id.value()),
                        }),
                    )
                    .await?;

                if !has_stopped_threads {
                    return Ok(EventEffect::Ignored);
                }
                let thread = match thread {
                    ThreadSet::All => StoppedThreadField::All,
                    ThreadSet::One(_) => {
                        StoppedThreadField::One(self.global_threads(sid, thread)?[0])
                    }
                    ThreadSet::Many(_) => StoppedThreadField::Unlisted,
                };
                Ok(EventEffect::Stopped {
                    breakpoint,
                    thread: Some(thread),
                    stopped_threads: self.global_threads(sid, stopped_threads)?,
                })
            }
            DebuggerEventKind::ThreadGroupAdded { local_group_id } => {
                let global_group_id = self
                    .model
                    .register_thread_group(sid, local_group_id)
                    .await?;
                Ok(EventEffect::ThreadGroup { global_group_id })
            }
            DebuggerEventKind::ThreadGroupRemoved { local_group_id } => {
                let global_group_id = self.model.remove_thread_group(sid, local_group_id).await?;
                Ok(EventEffect::ThreadGroup { global_group_id })
            }
            DebuggerEventKind::ThreadGroupStarted {
                local_group_id,
                pid,
            } => {
                let global_group_id = self
                    .model
                    .start_thread_group(sid, local_group_id, *pid)
                    .await?;
                Ok(EventEffect::ThreadGroup { global_group_id })
            }
            DebuggerEventKind::ThreadGroupExited { local_group_id } => {
                let global_group_id = self.model.exit_thread_group(sid, local_group_id).await?;
                Ok(EventEffect::ThreadGroup { global_group_id })
            }
            DebuggerEventKind::Unknown => {
                trace!(message = %event.message, "unhandled debugger event");
                Ok(EventEffect::Ignored)
            }
        }
    }

    async fn update_thread_statuses(
        &self,
        sid: u64,
        threads: &ThreadSet,
        status: ThreadStatus,
        location: Option<&crate::state::ThreadLocation>,
    ) -> Result<()> {
        match threads {
            ThreadSet::All => self.model.mark_all_threads(sid, status).await?,
            ThreadSet::One(local_thread_id) => {
                self.model
                    .update_thread_statuses_with_details(
                        sid,
                        &[*local_thread_id],
                        status,
                        location
                            .cloned()
                            .map(|location| (*local_thread_id, location)),
                        None,
                    )
                    .await?
            }
            ThreadSet::Many(local_thread_ids) => {
                self.model
                    .update_thread_statuses_with_details(sid, local_thread_ids, status, None, None)
                    .await?
            }
        }
        Ok(())
    }

    fn global_threads(&self, sid: u64, threads: &ThreadSet) -> Result<Vec<GlobalThreadId>> {
        match threads {
            ThreadSet::All => Ok(self.model.global_thread_ids_for_session(sid)),
            ThreadSet::One(local_thread_id) => Ok(vec![self
                .model
                .global_thread_id(sid, *local_thread_id)
                .ok_or_else(|| {
                    anyhow!(
                        "unknown thread {} while projecting session {} event",
                        local_thread_id,
                        sid
                    )
                })?]),
            ThreadSet::Many(local_thread_ids) => local_thread_ids
                .iter()
                .map(|local_thread_id| {
                    self.model
                        .global_thread_id(sid, *local_thread_id)
                        .ok_or_else(|| {
                            anyhow!(
                                "unknown thread {} while projecting session {} event",
                                local_thread_id,
                                sid
                            )
                        })
                })
                .collect(),
        }
    }
}

fn stop_kind(reasons: &[String]) -> ThreadStopKind {
    // Preserve unknown reasons without guessing from a source location.
    match reasons.first().map(String::as_str) {
        Some("breakpoint-hit") => ThreadStopKind::Breakpoint,
        Some("watchpoint-trigger" | "read-watchpoint-trigger" | "access-watchpoint-trigger") => {
            ThreadStopKind::Watchpoint
        }
        Some("end-stepping-range" | "function-finished" | "location-reached") => {
            ThreadStopKind::Step
        }
        Some("signal-received") => ThreadStopKind::Signal,
        Some("exception-received") => ThreadStopKind::Exception,
        Some("interrupt" | "paused") => ThreadStopKind::Pause,
        Some("entry") => ThreadStopKind::Entry,
        _ => ThreadStopKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::debugger::protocol::{Dict, Value};

    use super::super::decode_event;
    use super::*;
    use crate::notification::NotificationManager;

    fn payload(entries: &[(&str, Value)]) -> Dict {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<HashMap<_, _>>()
            .into()
    }

    fn test_reducer(model: Arc<RuntimeModel>) -> Arc<DebuggerEventReducer> {
        DebuggerEventReducer::new(
            model,
            BreakpointEventPublisher::new(
                Arc::new(NotificationManager::new()),
                crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
                crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
            ),
        )
    }

    #[tokio::test]
    async fn reducer_projects_thread_lifecycle_into_its_owned_model() {
        let model = RuntimeModel::new();
        model.register_session(7, "svc", None).await;
        let reducer = test_reducer(Arc::clone(&model));

        let added = decode_event(
            None,
            "thread-group-added".into(),
            payload(&[("id", "i1".into())]),
        )
        .unwrap();
        let added = reducer.project(added, 7).await.unwrap();
        let global_group_id = model.global_thread_group_id(7, "i1").unwrap();
        assert_eq!(
            added.output.unwrap().records[0].payload.as_ref().unwrap()["id"]
                .expect_string_ref()
                .unwrap(),
            global_group_id.to_string()
        );

        let created = decode_event(
            None,
            "thread-created".into(),
            payload(&[("id", "3".into()), ("group-id", "i1".into())]),
        )
        .unwrap();
        reducer.project(created, 7).await.unwrap();
        let global_thread_id = model.global_thread_id(7, 3).unwrap();

        let exited = decode_event(
            None,
            "thread-exited".into(),
            payload(&[("id", "3".into()), ("group-id", "i1".into())]),
        )
        .unwrap();
        let exited = reducer.project(exited, 7).await.unwrap();
        assert_eq!(
            exited.output.unwrap().records[0].payload.as_ref().unwrap()["id"]
                .expect_string_ref()
                .unwrap(),
            global_thread_id.to_string()
        );
        assert_eq!(model.global_thread_id(7, 3), None);
        assert_eq!(model.session_thread_group(7, 3).await, Some(None));
    }

    #[tokio::test]
    async fn stops_retain_reason_and_principal_thread_until_resume() {
        let model = RuntimeModel::new();
        model.register_session(7, "svc", None).await;
        model.register_thread_group(7, "i1").await.unwrap();
        model.register_thread(7, 3, "i1").await.unwrap();
        model.register_thread(7, 4, "i1").await.unwrap();
        let principal = model.global_thread_id(7, 3).unwrap().value();
        let breakpoint = model
            .insert_breakpoint(
                crate::state::BkptLoc::new("main.c", 5),
                crate::state::BreakpointProperties::default(),
                vec![crate::state::SubBkptSpec::Session {
                    sid: 7,
                    local_id: 2,
                }],
            )
            .unwrap();
        let reducer = test_reducer(Arc::clone(&model));
        for (reason, kind) in [
            ("breakpoint-hit", ThreadStopKind::Breakpoint),
            ("end-stepping-range", ThreadStopKind::Step),
            ("function-finished", ThreadStopKind::Step),
            ("signal-received", ThreadStopKind::Signal),
            ("unknown-backend-reason", ThreadStopKind::Other),
        ] {
            reducer
                .project(
                    decode_event(
                        None,
                        "running".into(),
                        payload(&[("thread-id", "all".into())]),
                    )
                    .unwrap(),
                    7,
                )
                .await
                .unwrap();
            let running = model.thread_snapshots_for_sessions(&[7]).await;
            assert!(running.iter().all(|thread| thread.stop_reason.is_none()));
            reducer
                .project(
                    decode_event(
                        None,
                        "stopped".into(),
                        payload(&[
                            ("reason", reason.into()),
                            ("signal-name", "SIGUSR1".into()),
                            ("thread-id", "3".into()),
                            ("stopped-threads", "all".into()),
                            ("bkptno", "2".into()),
                        ]),
                    )
                    .unwrap(),
                    7,
                )
                .await
                .unwrap();
            let stopped = model.thread_snapshots_for_sessions(&[7]).await;
            assert_eq!(stopped.len(), 2);
            for thread in stopped {
                assert_eq!(thread.status, ThreadStatus::STOPPED);
                let details = thread.stop_reason.unwrap();
                assert_eq!(details.kind, kind);
                assert_eq!(details.thread_id, Some(principal));
                assert_eq!(
                    details.breakpoint_id,
                    (kind == ThreadStopKind::Breakpoint).then(|| breakpoint.id())
                );
                assert_eq!(
                    details.signal_name.as_deref(),
                    (kind == ThreadStopKind::Signal).then_some("SIGUSR1")
                );
            }
        }
    }

    #[tokio::test]
    async fn stop_without_stopped_threads_still_updates_canonical_state() {
        let model = RuntimeModel::new();
        model.register_session(7, "svc", None).await;
        model.register_thread_group(7, "i1").await.unwrap();
        model.register_thread(7, 3, "i1").await.unwrap();
        let reducer = test_reducer(Arc::clone(&model));
        reducer
            .project(
                decode_event(
                    None,
                    "stopped".into(),
                    payload(&[
                        ("reason", "end-stepping-range".into()),
                        ("thread-id", "3".into()),
                    ]),
                )
                .unwrap(),
                7,
            )
            .await
            .unwrap();
        let stopped = model.thread_snapshots_for_sessions(&[7]).await;
        assert_eq!(
            stopped[0].stop_reason.as_ref().unwrap().kind,
            ThreadStopKind::Step
        );
    }

    #[tokio::test]
    async fn reducers_do_not_share_runtime_state() {
        let first_model = RuntimeModel::new();
        let second_model = RuntimeModel::new();
        first_model.register_session(1, "first", None).await;
        second_model.register_session(1, "second", None).await;

        let event = decode_event(
            None,
            "thread-group-added".into(),
            payload(&[("id", "i1".into())]),
        )
        .unwrap();
        test_reducer(Arc::clone(&first_model))
            .project(event, 1)
            .await
            .unwrap();

        assert!(first_model.global_thread_group_id(1, "i1").is_some());
        assert_eq!(second_model.global_thread_group_id(1, "i1"), None);
    }

    #[tokio::test]
    async fn stop_without_thread_passes_the_original_payload_through() {
        let model = RuntimeModel::new();
        model.register_session(7, "svc", None).await;
        let reducer = test_reducer(model);

        let stopped = decode_event(
            None,
            "stopped".into(),
            payload(&[("reason", "signal-received".into())]),
        )
        .unwrap();
        let projection = reducer.project(stopped, 7).await.unwrap();

        let record = &projection.output.unwrap().records[0];
        assert_eq!(record.prefix, "*");
        assert_eq!(
            record.payload.as_ref().unwrap()["reason"]
                .expect_string_ref()
                .unwrap(),
            "signal-received"
        );
    }

    #[tokio::test]
    async fn exit_reasons_terminate_the_session_instead_of_emitting_output() {
        let model = RuntimeModel::new();
        model.register_session(7, "svc", None).await;
        let reducer = test_reducer(model);

        let stopped = decode_event(
            None,
            "stopped".into(),
            payload(&[("reason", "exited-normally".into())]),
        )
        .unwrap();
        let projection = reducer.project(stopped, 7).await.unwrap();

        assert!(projection.output.is_none());
        assert!(projection.lifecycle.is_some());
    }
}
