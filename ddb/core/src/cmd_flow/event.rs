use anyhow::{anyhow, bail, Context, Result};
use gdbmi::{raw::Dict, Token};
use tracing::{debug, trace, warn};

use crate::{
    get_dbg_mgr,
    state::{get_bkpt_mgr, ThreadStatus, STATES},
};

use super::{
    emit_static, GenericStopAsyncRecordFormatter, ParsedSessionResponse,
    RunningAsyncRecordFormatter, StopAsyncRecordFormatter, ThreadCreatedNotifFormatter,
    ThreadExitedNotifFormatter, ThreadGroupNotifFormatter,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum ThreadSet {
    All,
    One(u64),
    Many(Vec<u64>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum DebuggerEventKind {
    BreakpointModified,
    BreakpointDeleted {
        local_breakpoint_id: u64,
    },
    ThreadCreated {
        local_thread_id: u64,
        local_group_id: String,
    },
    ThreadExited {
        local_thread_id: u64,
        local_group_id: String,
    },
    Running {
        threads: ThreadSet,
    },
    Stopped {
        reasons: Vec<String>,
        thread: Option<ThreadSet>,
        stopped_threads: Option<ThreadSet>,
    },
    ThreadGroupAdded {
        local_group_id: String,
    },
    ThreadGroupRemoved {
        local_group_id: String,
    },
    ThreadGroupStarted {
        local_group_id: String,
        pid: u64,
    },
    ThreadGroupExited {
        local_group_id: String,
    },
    Unknown,
}

#[derive(Debug, Clone)]
pub(crate) struct DebuggerEvent {
    pub token: Option<u64>,
    pub message: String,
    pub payload: Dict,
    pub kind: DebuggerEventKind,
}

fn required_string(payload: &Dict, key: &str) -> Result<String> {
    payload
        .get(key)
        .ok_or_else(|| anyhow!("missing '{}' field", key))?
        .expect_string_ref()
        .map(str::to_string)
        .map_err(|_| anyhow!("'{}' must be a string", key))
}

fn required_u64(payload: &Dict, key: &str) -> Result<u64> {
    required_string(payload, key)?
        .parse::<u64>()
        .with_context(|| format!("'{}' must be an unsigned integer", key))
}

fn parse_thread_set(value: &gdbmi::raw::Value, key: &str) -> Result<ThreadSet> {
    if let Ok(value) = value.expect_string_ref() {
        if value == "all" {
            return Ok(ThreadSet::All);
        }
        return value
            .parse::<u64>()
            .map(ThreadSet::One)
            .with_context(|| format!("'{}' contains an invalid thread id", key));
    }

    if let Ok(values) = value.expect_list_ref() {
        let threads = values
            .iter()
            .map(|value| {
                value
                    .expect_string_ref()
                    .map_err(|_| anyhow!("'{}' thread ids must be strings", key))?
                    .parse::<u64>()
                    .with_context(|| format!("'{}' contains an invalid thread id", key))
            })
            .collect::<Result<Vec<_>>>()?;
        return Ok(ThreadSet::Many(threads));
    }

    bail!("'{}' must be a thread id, 'all', or a list", key)
}

fn parse_reasons(payload: &Dict) -> Result<Vec<String>> {
    let Some(reason) = payload.get("reason") else {
        return Ok(Vec::new());
    };
    if let Ok(reason) = reason.expect_string_ref() {
        return Ok(vec![reason.to_string()]);
    }
    if let Ok(reasons) = reason.expect_list_ref() {
        return reasons
            .iter()
            .map(|reason| {
                reason
                    .expect_string_ref()
                    .map(str::to_string)
                    .map_err(|_| anyhow!("'reason' list values must be strings"))
            })
            .collect();
    }
    bail!("'reason' must be a string or list of strings")
}

pub(crate) fn decode_event(
    token: Option<Token>,
    message: String,
    payload: Dict,
) -> Result<DebuggerEvent> {
    let kind = match message.as_str() {
        "breakpoint-modified" => DebuggerEventKind::BreakpointModified,
        "breakpoint-deleted" => DebuggerEventKind::BreakpointDeleted {
            local_breakpoint_id: required_u64(&payload, "id")?,
        },
        "thread-created" => DebuggerEventKind::ThreadCreated {
            local_thread_id: required_u64(&payload, "id")?,
            local_group_id: required_string(&payload, "group-id")?,
        },
        "thread-exited" => DebuggerEventKind::ThreadExited {
            local_thread_id: required_u64(&payload, "id")?,
            local_group_id: required_string(&payload, "group-id")?,
        },
        "running" => DebuggerEventKind::Running {
            threads: parse_thread_set(
                payload
                    .get("thread-id")
                    .ok_or_else(|| anyhow!("missing 'thread-id' field"))?,
                "thread-id",
            )?,
        },
        "stopped" => DebuggerEventKind::Stopped {
            reasons: parse_reasons(&payload)?,
            thread: payload
                .get("thread-id")
                .map(|value| parse_thread_set(value, "thread-id"))
                .transpose()?,
            stopped_threads: payload
                .get("stopped-threads")
                .map(|value| parse_thread_set(value, "stopped-threads"))
                .transpose()?,
        },
        "thread-group-added" => DebuggerEventKind::ThreadGroupAdded {
            local_group_id: required_string(&payload, "id")?,
        },
        "thread-group-removed" => DebuggerEventKind::ThreadGroupRemoved {
            local_group_id: required_string(&payload, "id")?,
        },
        "thread-group-started" => DebuggerEventKind::ThreadGroupStarted {
            local_group_id: required_string(&payload, "id")?,
            pid: required_u64(&payload, "pid")?,
        },
        "thread-group-exited" => DebuggerEventKind::ThreadGroupExited {
            local_group_id: required_string(&payload, "id")?,
        },
        _ => DebuggerEventKind::Unknown,
    };

    Ok(DebuggerEvent {
        token: token.map(|token| token.0 as u64),
        message,
        payload,
        kind,
    })
}

pub(crate) async fn project_event(event: DebuggerEvent, sid: u64) -> Result<()> {
    let DebuggerEvent {
        token,
        message,
        payload,
        kind,
    } = event;

    match kind {
        DebuggerEventKind::BreakpointModified => {}
        DebuggerEventKind::BreakpointDeleted {
            local_breakpoint_id,
        } => {
            get_bkpt_mgr()
                .delete_local_bkpt(sid, local_breakpoint_id)
                .await;
        }
        DebuggerEventKind::ThreadCreated {
            local_thread_id,
            local_group_id,
        } => {
            let (gtid, gtgid) = STATES
                .create_thread(sid, local_thread_id, &local_group_id)
                .await;
            let service_meta = STATES.session_service_meta(sid).await;
            debug!(?service_meta, "projected thread-created event");
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                ThreadCreatedNotifFormatter::new(gtid, gtgid, sid, service_meta),
            );
        }
        DebuggerEventKind::ThreadExited {
            local_thread_id,
            local_group_id,
        } => {
            let gtid = STATES.remove_thread(sid, local_thread_id).ok_or_else(|| {
                anyhow!(
                    "thread exit references unknown thread: sid {}, tid {}",
                    sid,
                    local_thread_id
                )
            })?;
            let gtgid = STATES
                .global_thread_group_id(sid, &local_group_id)
                .ok_or_else(|| {
                    anyhow!(
                        "thread exit references unknown group: sid {}, group {}",
                        sid,
                        local_group_id
                    )
                })?;
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                ThreadExitedNotifFormatter::new(gtid, gtgid, sid),
            );
        }
        DebuggerEventKind::Running { threads } => {
            let all_running = matches!(threads, ThreadSet::All);
            match threads {
                ThreadSet::All => {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::RUNNING)
                        .await;
                }
                ThreadSet::One(local_thread_id) => {
                    STATES
                        .update_thread_status(sid, local_thread_id, ThreadStatus::RUNNING)
                        .await;
                }
                ThreadSet::Many(local_thread_ids) => {
                    for local_thread_id in local_thread_ids {
                        STATES
                            .update_thread_status(sid, local_thread_id, ThreadStatus::RUNNING)
                            .await;
                    }
                }
            }
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                RunningAsyncRecordFormatter::new(all_running),
            );
        }
        DebuggerEventKind::Stopped {
            reasons,
            thread,
            stopped_threads,
        } => {
            let is_exit = reasons.iter().any(|reason| reason.contains("exit"));
            let is_breakpoint = reasons.iter().any(|reason| reason == "breakpoint-hit");
            if is_exit {
                tokio::spawn(async move {
                    get_dbg_mgr().remove_session(sid).await;
                });
                return Ok(());
            }

            match thread {
                Some(ThreadSet::All) => {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::STOPPED)
                        .await;
                }
                Some(ThreadSet::One(local_thread_id)) => {
                    STATES
                        .update_thread_status(sid, local_thread_id, ThreadStatus::STOPPED)
                        .await;
                    if is_breakpoint {
                        STATES.select_local_thread(sid, local_thread_id).await;
                    }
                }
                Some(ThreadSet::Many(local_thread_ids)) => {
                    for local_thread_id in local_thread_ids {
                        STATES
                            .update_thread_status(sid, local_thread_id, ThreadStatus::STOPPED)
                            .await;
                    }
                }
                None => {
                    let response = ParsedSessionResponse::new(sid, message, Some(payload));
                    emit_static(
                        response.to_finished_cmd(token, sid),
                        GenericStopAsyncRecordFormatter,
                    );
                    return Ok(());
                }
            }

            match stopped_threads {
                Some(ThreadSet::All) => {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::STOPPED)
                        .await;
                }
                Some(ThreadSet::One(local_thread_id)) => {
                    STATES
                        .update_thread_status(sid, local_thread_id, ThreadStatus::STOPPED)
                        .await;
                }
                Some(ThreadSet::Many(local_thread_ids)) => {
                    for local_thread_id in local_thread_ids {
                        STATES
                            .update_thread_status(sid, local_thread_id, ThreadStatus::STOPPED)
                            .await;
                    }
                }
                None => {
                    warn!(sid, ?payload, "stopped event is missing stopped-threads");
                    return Ok(());
                }
            }

            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                StopAsyncRecordFormatter,
            );
        }
        DebuggerEventKind::ThreadGroupAdded { local_group_id } => {
            let gtgid = STATES.add_thread_group(sid, &local_group_id).await;
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        DebuggerEventKind::ThreadGroupRemoved { local_group_id } => {
            let gtgid = STATES
                .remove_thread_group(sid, &local_group_id)
                .await
                .ok_or_else(|| {
                    anyhow!("unknown thread group {} in session {}", local_group_id, sid)
                })?;
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        DebuggerEventKind::ThreadGroupStarted {
            local_group_id,
            pid,
        } => {
            let gtgid = STATES
                .start_thread_group(sid, &local_group_id, pid)
                .await
                .ok_or_else(|| {
                    anyhow!("unknown thread group {} in session {}", local_group_id, sid)
                })?;
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        DebuggerEventKind::ThreadGroupExited { local_group_id } => {
            let gtgid = STATES
                .exit_thread_group(sid, &local_group_id)
                .await
                .ok_or_else(|| {
                    anyhow!("unknown thread group {} in session {}", local_group_id, sid)
                })?;
            let response = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                response.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        DebuggerEventKind::Unknown => trace!(%message, "unhandled debugger event"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gdbmi::raw::Value;

    use super::*;

    fn payload(entries: &[(&str, Value)]) -> Dict {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<HashMap<_, _>>()
            .into()
    }

    #[test]
    fn decodes_thread_lifecycle_events() {
        let created = decode_event(
            None,
            "thread-created".into(),
            payload(&[("id", "7".into()), ("group-id", "i3".into())]),
        )
        .unwrap();
        assert_eq!(
            created.kind,
            DebuggerEventKind::ThreadCreated {
                local_thread_id: 7,
                local_group_id: "i3".into(),
            }
        );

        let exited = decode_event(
            None,
            "thread-exited".into(),
            payload(&[("id", "7".into()), ("group-id", "i3".into())]),
        )
        .unwrap();
        assert!(matches!(
            exited.kind,
            DebuggerEventKind::ThreadExited { .. }
        ));
    }

    #[test]
    fn decodes_running_and_stopped_thread_sets() {
        let running = decode_event(
            None,
            "running".into(),
            payload(&[("thread-id", "all".into())]),
        )
        .unwrap();
        assert_eq!(
            running.kind,
            DebuggerEventKind::Running {
                threads: ThreadSet::All
            }
        );

        let stopped = decode_event(
            None,
            "stopped".into(),
            payload(&[
                ("reason", "breakpoint-hit".into()),
                ("thread-id", "4".into()),
                ("stopped-threads", Value::List(vec!["4".into(), "5".into()])),
            ]),
        )
        .unwrap();
        assert_eq!(
            stopped.kind,
            DebuggerEventKind::Stopped {
                reasons: vec!["breakpoint-hit".into()],
                thread: Some(ThreadSet::One(4)),
                stopped_threads: Some(ThreadSet::Many(vec![4, 5])),
            }
        );
    }

    #[test]
    fn decodes_group_and_breakpoint_events() {
        let started = decode_event(
            None,
            "thread-group-started".into(),
            payload(&[("id", "i1".into()), ("pid", "42".into())]),
        )
        .unwrap();
        assert_eq!(
            started.kind,
            DebuggerEventKind::ThreadGroupStarted {
                local_group_id: "i1".into(),
                pid: 42,
            }
        );

        let deleted = decode_event(
            None,
            "breakpoint-deleted".into(),
            payload(&[("id", "9".into())]),
        )
        .unwrap();
        assert_eq!(
            deleted.kind,
            DebuggerEventKind::BreakpointDeleted {
                local_breakpoint_id: 9
            }
        );
    }

    #[test]
    fn preserves_unknown_events_without_guessing_their_schema() {
        let event = decode_event(
            Some(Token(12)),
            "library-loaded".into(),
            payload(&[("id", "libexample.so".into())]),
        )
        .unwrap();
        assert_eq!(event.token, Some(12));
        assert_eq!(event.kind, DebuggerEventKind::Unknown);
    }

    #[test]
    fn rejects_malformed_known_events() {
        let missing_id = decode_event(
            None,
            "thread-created".into(),
            Dict::new(HashMap::new()),
        );
        assert!(missing_id.unwrap_err().to_string().contains("missing 'id'"));

        let invalid_thread = decode_event(
            None,
            "running".into(),
            payload(&[("thread-id", "worker-1".into())]),
        );
        assert!(invalid_thread
            .unwrap_err()
            .to_string()
            .contains("invalid thread id"));
    }
}
