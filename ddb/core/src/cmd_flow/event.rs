use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use gdbmi::{raw::Dict, Token};
use tracing::trace;

use crate::state::{get_bkpt_mgr, get_group_mgr, ThreadStatus, STATES};

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

#[derive(Debug, Clone)]
pub(crate) struct ProjectedDebuggerRecord {
    pub prefix: &'static str,
    pub message: String,
    pub payload: Option<Dict>,
    pub token: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectedDebuggerOutput {
    pub records: Vec<ProjectedDebuggerRecord>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum SessionLifecycleEffect {
    Exited { reasons: Vec<String> },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EventProjection {
    pub output: Option<ProjectedDebuggerOutput>,
    pub lifecycle: Option<SessionLifecycleEffect>,
}

impl EventProjection {
    fn record(prefix: &'static str, message: String, payload: Dict, token: Option<u64>) -> Self {
        Self {
            output: Some(ProjectedDebuggerOutput {
                records: vec![ProjectedDebuggerRecord {
                    prefix,
                    message,
                    payload: Some(payload),
                    token,
                }],
            }),
            lifecycle: None,
        }
    }

    fn records(records: Vec<ProjectedDebuggerRecord>) -> Self {
        Self {
            output: (!records.is_empty()).then_some(ProjectedDebuggerOutput { records }),
            lifecycle: None,
        }
    }

    fn exited(reasons: Vec<String>) -> Self {
        Self {
            output: None,
            lifecycle: Some(SessionLifecycleEffect::Exited { reasons }),
        }
    }
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

fn global_threads(sid: u64, threads: &ThreadSet) -> Result<Vec<u64>> {
    match threads {
        ThreadSet::All => Ok(STATES.global_thread_ids_for_session(sid)),
        ThreadSet::One(local_thread_id) => Ok(vec![STATES
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
                STATES
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

pub(crate) async fn project_event(event: DebuggerEvent, sid: u64) -> Result<EventProjection> {
    let DebuggerEvent {
        token,
        message,
        payload,
        kind,
    } = event;

    match kind {
        DebuggerEventKind::BreakpointModified => Ok(EventProjection::default()),
        DebuggerEventKind::BreakpointDeleted {
            local_breakpoint_id,
        } => {
            get_bkpt_mgr()
                .delete_local_bkpt(sid, local_breakpoint_id)
                .await;
            Ok(EventProjection::default())
        }
        DebuggerEventKind::ThreadCreated {
            local_thread_id,
            local_group_id,
        } => {
            let gtgid = STATES
                .global_thread_group_id(sid, &local_group_id)
                .ok_or_else(|| {
                    anyhow!(
                        "thread creation references unknown group: sid {}, group {}",
                        sid,
                        local_group_id
                    )
                })?;
            let (gtid, created_gtgid) = STATES
                .create_thread(sid, local_thread_id, &local_group_id)
                .await;
            debug_assert_eq!(created_gtgid, gtgid);
            let service_meta = STATES.session_service_meta(sid).await;
            let alias = service_meta
                .map(|meta| meta.alias)
                .unwrap_or_else(|| "UNKNOWN".to_string());
            let group_hash = get_group_mgr()
                .group_hash_by_session(sid)
                .unwrap_or_else(|| "UNKNOWN".to_string());
            let projected_payload = HashMap::from([
                ("id".to_string(), gtid.to_string().into()),
                ("group-id".to_string(), format!("i{}", gtgid).into()),
                ("group-hash".to_string(), group_hash.into()),
                ("session-id".to_string(), sid.to_string().into()),
                ("session-alias".to_string(), alias.into()),
            ])
            .into();
            Ok(EventProjection::record(
                "=",
                message,
                projected_payload,
                None,
            ))
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
            let projected_payload = HashMap::from([
                ("id".to_string(), gtid.to_string().into()),
                ("group-id".to_string(), format!("i{}", gtgid).into()),
                ("session-id".to_string(), sid.to_string().into()),
            ])
            .into();
            Ok(EventProjection::record(
                "=",
                message,
                projected_payload,
                None,
            ))
        }
        DebuggerEventKind::Running { threads } => {
            match &threads {
                ThreadSet::All => {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::RUNNING)
                        .await;
                }
                ThreadSet::One(local_thread_id) => {
                    STATES
                        .update_thread_status(sid, *local_thread_id, ThreadStatus::RUNNING)
                        .await;
                }
                ThreadSet::Many(local_thread_ids) => {
                    for local_thread_id in local_thread_ids {
                        STATES
                            .update_thread_status(sid, *local_thread_id, ThreadStatus::RUNNING)
                            .await;
                    }
                }
            }
            let records = global_threads(sid, &threads)?
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
            Ok(EventProjection::records(records))
        }
        DebuggerEventKind::Stopped {
            reasons,
            thread,
            stopped_threads,
        } => {
            let is_exit = reasons.iter().any(|reason| reason.contains("exit"));
            let is_breakpoint = reasons.iter().any(|reason| reason == "breakpoint-hit");
            if is_exit {
                return Ok(EventProjection::exited(reasons));
            }

            match &thread {
                Some(ThreadSet::All) => {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::STOPPED)
                        .await;
                }
                Some(ThreadSet::One(local_thread_id)) => {
                    STATES
                        .update_thread_status(sid, *local_thread_id, ThreadStatus::STOPPED)
                        .await;
                    if is_breakpoint {
                        STATES.select_local_thread(sid, *local_thread_id).await;
                    }
                }
                Some(ThreadSet::Many(local_thread_ids)) => {
                    for local_thread_id in local_thread_ids {
                        STATES
                            .update_thread_status(sid, *local_thread_id, ThreadStatus::STOPPED)
                            .await;
                    }
                }
                None => {
                    return Ok(EventProjection::record("*", message, payload, token));
                }
            }

            match &stopped_threads {
                Some(ThreadSet::All) => {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::STOPPED)
                        .await;
                }
                Some(ThreadSet::One(local_thread_id)) => {
                    STATES
                        .update_thread_status(sid, *local_thread_id, ThreadStatus::STOPPED)
                        .await;
                }
                Some(ThreadSet::Many(local_thread_ids)) => {
                    for local_thread_id in local_thread_ids {
                        STATES
                            .update_thread_status(sid, *local_thread_id, ThreadStatus::STOPPED)
                            .await;
                    }
                }
                None => {
                    return Ok(EventProjection::default());
                }
            }

            let mut projected_payload = payload;
            if is_breakpoint {
                if let Some(local_breakpoint_id) = projected_payload
                    .get("bkptno")
                    .and_then(|value| value.expect_string_ref().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    if let Some((breakpoint_id, subbreakpoint_id)) =
                        get_bkpt_mgr().breakpoint_ids_by_local_id(sid, local_breakpoint_id)
                    {
                        projected_payload.insert("bkptno".into(), breakpoint_id.to_string().into());
                        projected_payload
                            .insert("subbkptno".into(), subbreakpoint_id.to_string().into());
                    }
                }
            }
            if let Some(thread) = &thread {
                match thread {
                    ThreadSet::All => {
                        projected_payload.insert("thread-id".into(), "all".into());
                    }
                    ThreadSet::One(_) => {
                        let global_thread_id = global_threads(sid, thread)?[0];
                        projected_payload
                            .insert("thread-id".into(), global_thread_id.to_string().into());
                    }
                    ThreadSet::Many(_) => {}
                }
            }
            if let Some(stopped_threads) = &stopped_threads {
                let stopped_threads = global_threads(sid, stopped_threads)?
                    .into_iter()
                    .map(|thread_id| thread_id.to_string().into())
                    .collect();
                projected_payload.insert(
                    "stopped-threads".into(),
                    gdbmi::raw::Value::List(stopped_threads),
                );
            }
            projected_payload.insert("session-id".into(), sid.to_string().into());
            Ok(EventProjection::record(
                "*",
                message,
                projected_payload,
                token,
            ))
        }
        DebuggerEventKind::ThreadGroupAdded { local_group_id } => {
            let gtgid = STATES.add_thread_group(sid, &local_group_id).await;
            let mut projected_payload = payload;
            projected_payload.insert("id".into(), gtgid.to_string().into());
            Ok(EventProjection::record(
                "=",
                message,
                projected_payload,
                token,
            ))
        }
        DebuggerEventKind::ThreadGroupRemoved { local_group_id } => {
            let gtgid = STATES
                .remove_thread_group(sid, &local_group_id)
                .await
                .ok_or_else(|| {
                    anyhow!("unknown thread group {} in session {}", local_group_id, sid)
                })?;
            let mut projected_payload = payload;
            projected_payload.insert("id".into(), gtgid.to_string().into());
            Ok(EventProjection::record(
                "=",
                message,
                projected_payload,
                token,
            ))
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
            let mut projected_payload = payload;
            projected_payload.insert("id".into(), gtgid.to_string().into());
            Ok(EventProjection::record(
                "=",
                message,
                projected_payload,
                token,
            ))
        }
        DebuggerEventKind::ThreadGroupExited { local_group_id } => {
            let gtgid = STATES
                .exit_thread_group(sid, &local_group_id)
                .await
                .ok_or_else(|| {
                    anyhow!("unknown thread group {} in session {}", local_group_id, sid)
                })?;
            let mut projected_payload = payload;
            projected_payload.insert("id".into(), gtgid.to_string().into());
            Ok(EventProjection::record(
                "=",
                message,
                projected_payload,
                token,
            ))
        }
        DebuggerEventKind::Unknown => {
            trace!(%message, "unhandled debugger event");
            Ok(EventProjection::default())
        }
    }
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
        let missing_id = decode_event(None, "thread-created".into(), Dict::new(HashMap::new()));
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
