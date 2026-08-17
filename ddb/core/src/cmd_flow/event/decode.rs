//! Pure decoding of raw debugger notifications into typed events.

use crate::debugger::protocol::{Dict, Value};
use anyhow::{anyhow, bail, Context, Result};

use super::{DebuggerEvent, DebuggerEventKind, ThreadSet};
use crate::state::ThreadLocation;

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

fn optional_lenient_u64(payload: &Dict, key: &str) -> Option<u64> {
    payload
        .get(key)
        .and_then(|value| value.expect_string_ref().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn parse_thread_set(value: &Value, key: &str) -> Result<ThreadSet> {
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

fn optional_string(payload: &Dict, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value.expect_string_ref().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn stopped_location(payload: &Dict) -> Option<ThreadLocation> {
    let frame = payload.get("frame")?.expect_dict_ref().ok()?;
    let path = optional_string(frame, "fullname").or_else(|| optional_string(frame, "file"));
    let line = optional_string(frame, "line").and_then(|line| line.parse().ok());
    let column = optional_string(frame, "column").and_then(|column| column.parse().ok());
    let address = optional_string(frame, "addr");
    let function_name = optional_string(frame, "func");
    (path.is_some() || line.is_some() || address.is_some() || function_name.is_some()).then_some(
        ThreadLocation {
            path,
            line,
            column,
            address,
            function_name,
        },
    )
}

pub(crate) fn decode_event(
    token: Option<u64>,
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
            local_breakpoint_id: optional_lenient_u64(&payload, "bkptno"),
            location: stopped_location(&payload),
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
        token,
        message,
        payload,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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
                ("bkptno", "2".into()),
            ]),
        )
        .unwrap();
        assert_eq!(
            stopped.kind,
            DebuggerEventKind::Stopped {
                reasons: vec!["breakpoint-hit".into()],
                thread: Some(ThreadSet::One(4)),
                stopped_threads: Some(ThreadSet::Many(vec![4, 5])),
                local_breakpoint_id: Some(2),
                location: None,
            }
        );
    }

    #[test]
    fn stopped_breakpoint_number_is_decoded_leniently() {
        let stopped = decode_event(
            None,
            "stopped".into(),
            payload(&[
                ("reason", "breakpoint-hit".into()),
                ("thread-id", "4".into()),
                ("bkptno", "not-a-number".into()),
            ]),
        )
        .unwrap();
        assert!(matches!(
            stopped.kind,
            DebuggerEventKind::Stopped {
                local_breakpoint_id: None,
                ..
            }
        ));
    }

    #[test]
    fn stopped_frame_is_decoded_into_a_backend_neutral_location() {
        let frame = payload(&[
            ("addr", "0x401020".into()),
            ("func", "handle_request".into()),
            ("file", "main.rs".into()),
            ("fullname", "/workspace/src/main.rs".into()),
            ("line", "42".into()),
            ("column", "7".into()),
        ]);
        let stopped = decode_event(
            None,
            "stopped".into(),
            payload(&[
                ("thread-id", "4".into()),
                ("stopped-threads", "all".into()),
                ("frame", Value::Dict(frame)),
            ]),
        )
        .unwrap();

        assert!(matches!(
            stopped.kind,
            DebuggerEventKind::Stopped {
                location: Some(ThreadLocation {
                    path: Some(path),
                    line: Some(42),
                    column: Some(7),
                    address: Some(address),
                    function_name: Some(function),
                }),
                ..
            } if path == "/workspace/src/main.rs"
                && address == "0x401020"
                && function == "handle_request"
        ));
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
            Some(12),
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
