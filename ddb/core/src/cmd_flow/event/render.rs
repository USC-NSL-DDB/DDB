//! Pure rendering of applied event effects into MI records.
//!
//! Nothing here touches the runtime model: every id translation the output
//! needs is captured in [`EventEffect`] when the event is applied.

use std::collections::HashMap;

use crate::state::{GlobalThreadGroupId, GlobalThreadId, GlobalThreadIdentity};

use super::{DebuggerEvent, EventProjection, ProjectedDebuggerRecord};

/// Outcome of applying a debugger event to the runtime model, carrying the
/// translated identities the rendered output refers to.
#[derive(Debug, Clone)]
pub(super) enum EventEffect {
    /// The event produced no output for the client.
    Ignored,
    /// The event is forwarded with its payload untouched.
    Passthrough,
    /// The debuggee exited; the session lifecycle takes over.
    Exited {
        reasons: Vec<String>,
    },
    ThreadCreated {
        identity: GlobalThreadIdentity,
        alias: String,
        group_hash: String,
    },
    ThreadExited {
        identity: GlobalThreadIdentity,
    },
    Running {
        global_threads: Vec<GlobalThreadId>,
    },
    Stopped {
        breakpoint: Option<(u64, u64)>,
        thread: Option<StoppedThreadField>,
        stopped_threads: Vec<GlobalThreadId>,
    },
    ThreadGroup {
        global_group_id: GlobalThreadGroupId,
    },
}

/// How the `thread-id` field of a stop record is rewritten.
#[derive(Debug, Clone)]
pub(super) enum StoppedThreadField {
    All,
    One(GlobalThreadId),
    Unlisted,
}

pub(super) fn render(effect: EventEffect, event: DebuggerEvent, sid: u64) -> EventProjection {
    let DebuggerEvent {
        token,
        message,
        payload,
        ..
    } = event;

    match effect {
        EventEffect::Ignored => EventProjection::default(),
        EventEffect::Passthrough => EventProjection::record("*", message, payload, token),
        EventEffect::Exited { reasons } => EventProjection::exited(reasons),
        EventEffect::ThreadCreated {
            identity,
            alias,
            group_hash,
        } => {
            let projected_payload = HashMap::from([
                ("id".to_string(), identity.thread_id.to_string().into()),
                (
                    "group-id".to_string(),
                    format!("i{}", identity.thread_group_id).into(),
                ),
                ("group-hash".to_string(), group_hash.into()),
                ("session-id".to_string(), sid.to_string().into()),
                ("session-alias".to_string(), alias.into()),
            ])
            .into();
            EventProjection::record("=", message, projected_payload, None)
        }
        EventEffect::ThreadExited { identity } => {
            let projected_payload = HashMap::from([
                ("id".to_string(), identity.thread_id.to_string().into()),
                (
                    "group-id".to_string(),
                    format!("i{}", identity.thread_group_id).into(),
                ),
                ("session-id".to_string(), sid.to_string().into()),
            ])
            .into();
            EventProjection::record("=", message, projected_payload, None)
        }
        EventEffect::Running { global_threads } => {
            let records = global_threads
                .into_iter()
                .map(|global_thread_id| ProjectedDebuggerRecord {
                    prefix: "*",
                    message: message.clone(),
                    payload: Some(
                        HashMap::from([(
                            "thread-id".to_string(),
                            global_thread_id.to_string().into(),
                        )])
                        .into(),
                    ),
                    token: None,
                })
                .collect();
            EventProjection::records(records)
        }
        EventEffect::Stopped {
            breakpoint,
            thread,
            stopped_threads,
        } => {
            let mut projected_payload = payload;
            if let Some((breakpoint_id, subbreakpoint_id)) = breakpoint {
                projected_payload.insert("bkptno".into(), breakpoint_id.to_string().into());
                projected_payload.insert("subbkptno".into(), subbreakpoint_id.to_string().into());
            }
            match thread {
                Some(StoppedThreadField::All) => {
                    projected_payload.insert("thread-id".into(), "all".into());
                }
                Some(StoppedThreadField::One(global_thread_id)) => {
                    projected_payload
                        .insert("thread-id".into(), global_thread_id.to_string().into());
                }
                Some(StoppedThreadField::Unlisted) | None => {}
            }
            if !stopped_threads.is_empty() {
                let stopped_threads = stopped_threads
                    .into_iter()
                    .map(|thread_id| thread_id.to_string().into())
                    .collect();
                projected_payload.insert(
                    "stopped-threads".into(),
                    crate::debugger::protocol::Value::List(stopped_threads),
                );
            }
            projected_payload.insert("session-id".into(), sid.to_string().into());
            EventProjection::record("*", message, projected_payload, token)
        }
        EventEffect::ThreadGroup { global_group_id } => {
            let mut projected_payload = payload;
            projected_payload.insert("id".into(), global_group_id.to_string().into());
            EventProjection::record("=", message, projected_payload, token)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::debugger::protocol::Dict;

    use super::*;

    fn event(message: &str, payload: &[(&str, crate::debugger::protocol::Value)]) -> DebuggerEvent {
        DebuggerEvent {
            token: Some(9),
            message: message.to_string(),
            payload: payload
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect::<HashMap<_, _>>()
                .into(),
            kind: super::super::DebuggerEventKind::Unknown,
        }
    }

    #[test]
    fn stop_records_are_rewritten_with_global_identities() {
        let effect = EventEffect::Stopped {
            breakpoint: Some((3, 1)),
            thread: Some(StoppedThreadField::One(GlobalThreadId::new(11))),
            stopped_threads: vec![GlobalThreadId::new(11), GlobalThreadId::new(12)],
        };

        let projection = render(
            effect,
            event(
                "stopped",
                &[("bkptno", "2".into()), ("thread-id", "4".into())],
            ),
            7,
        );

        let record = &projection.output.unwrap().records[0];
        let payload = record.payload.as_ref().unwrap();
        assert_eq!(record.prefix, "*");
        assert_eq!(record.token, Some(9));
        assert_eq!(payload["bkptno"].expect_string_ref().unwrap(), "3");
        assert_eq!(payload["subbkptno"].expect_string_ref().unwrap(), "1");
        assert_eq!(payload["thread-id"].expect_string_ref().unwrap(), "11");
        assert_eq!(payload["session-id"].expect_string_ref().unwrap(), "7");
        assert_eq!(
            payload["stopped-threads"].expect_list_ref().unwrap().len(),
            2
        );
    }

    #[test]
    fn thread_created_records_are_built_from_the_applied_identity() {
        let effect = EventEffect::ThreadCreated {
            identity: GlobalThreadIdentity {
                thread_id: GlobalThreadId::new(5),
                thread_group_id: GlobalThreadGroupId::new(2),
            },
            alias: "api".to_string(),
            group_hash: "hash-a".to_string(),
        };

        let projection = render(effect, event("thread-created", &[]), 7);

        let record = &projection.output.unwrap().records[0];
        let payload = record.payload.as_ref().unwrap();
        assert_eq!(record.prefix, "=");
        assert_eq!(record.token, None);
        assert_eq!(payload["id"].expect_string_ref().unwrap(), "5");
        assert_eq!(payload["group-id"].expect_string_ref().unwrap(), "i2");
        assert_eq!(payload["group-hash"].expect_string_ref().unwrap(), "hash-a");
        assert_eq!(payload["session-alias"].expect_string_ref().unwrap(), "api");
    }

    #[test]
    fn ignored_and_passthrough_effects_preserve_or_suppress_the_payload() {
        let ignored = render(EventEffect::Ignored, event("stopped", &[]), 7);
        assert!(ignored.output.is_none());
        assert!(ignored.lifecycle.is_none());

        let passthrough = render(
            EventEffect::Passthrough,
            event("stopped", &[("reason", "signal-received".into())]),
            7,
        );
        let record = &passthrough.output.unwrap().records[0];
        let payload: &Dict = record.payload.as_ref().unwrap();
        assert_eq!(
            payload["reason"].expect_string_ref().unwrap(),
            "signal-received"
        );
    }
}
