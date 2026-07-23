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
    state::{GlobalThreadId, RuntimeModel, ThreadStatus},
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
        Ok(render::render(effect, event, sid))
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
                self.update_thread_statuses(sid, threads, ThreadStatus::RUNNING)
                    .await?;
                Ok(EventEffect::Running {
                    global_threads: self.global_threads(sid, threads)?,
                })
            }
            DebuggerEventKind::Stopped {
                reasons,
                thread,
                stopped_threads,
                local_breakpoint_id,
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
                self.update_thread_statuses(sid, thread, ThreadStatus::STOPPED)
                    .await?;
                if is_breakpoint {
                    if let ThreadSet::One(local_thread_id) = thread {
                        self.model
                            .select_local_thread(sid, *local_thread_id)
                            .await?;
                    }
                }

                let Some(stopped_threads) = stopped_threads else {
                    return Ok(EventEffect::Ignored);
                };
                self.update_thread_statuses(sid, stopped_threads, ThreadStatus::STOPPED)
                    .await?;

                let breakpoint = if is_breakpoint {
                    local_breakpoint_id.and_then(|local_breakpoint_id| {
                        self.model
                            .breakpoint_ids_by_local_id(sid, local_breakpoint_id)
                    })
                } else {
                    None
                };
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
    ) -> Result<()> {
        match threads {
            ThreadSet::All => self.model.mark_all_threads(sid, status).await?,
            ThreadSet::One(local_thread_id) => {
                self.model
                    .update_thread_statuses(sid, &[*local_thread_id], status)
                    .await?
            }
            ThreadSet::Many(local_thread_ids) => {
                self.model
                    .update_thread_statuses(sid, local_thread_ids, status)
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
