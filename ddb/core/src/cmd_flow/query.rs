//! Semantic projection for read-only debugger queries.
//!
//! Query responses enter this module with debugger-local identifiers and leave
//! with DDB-global identifiers. Presenters can therefore remain pure and every
//! ingress observes the same response semantics.

use std::fmt;

use gdbmi::raw::{Dict, Value};

use super::{decoder::DecodeError, FinishedCmd};
use crate::state::StateMgr;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum QueryProjectionError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("malformed {collection} record for session {sid}: {reason}")]
    MalformedRecord {
        sid: u64,
        collection: &'static str,
        reason: String,
    },
    #[error("session {sid} thread {local_id} has no global thread mapping")]
    UnknownThread { sid: u64, local_id: u64 },
    #[error("global thread {global_id} is unknown")]
    UnknownGlobalThread { global_id: u64 },
    #[error("session {sid} thread group '{local_id}' has no global group mapping")]
    UnknownThreadGroup { sid: u64, local_id: String },
}

#[derive(Clone, Copy)]
pub(crate) struct QueryProjector<'a> {
    state: &'a StateMgr,
}

impl fmt::Debug for QueryProjector<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("QueryProjector").finish()
    }
}

impl<'a> QueryProjector<'a> {
    pub(crate) fn new(state: &'a StateMgr) -> Self {
        Self { state }
    }

    pub(crate) fn resolve_thread(
        &self,
        global_id: u64,
    ) -> Result<(u64, u64), QueryProjectionError> {
        self.state
            .local_thread_id(global_id)
            .map(|local| local.into_parts())
            .ok_or(QueryProjectionError::UnknownGlobalThread { global_id })
    }

    pub(crate) fn project_threads(
        &self,
        mut completion: FinishedCmd,
    ) -> Result<FinishedCmd, QueryProjectionError> {
        let current_thread_id = self
            .state
            .current_thread_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let thread_ids = self.state.read_thread_ids();

        for response in completion.get_responses_mut() {
            let sid = response.get_sid();
            let payload = response
                .get_payload_mut()
                .ok_or(DecodeError::MissingPayload { sid })?;
            for value in list_mut(payload, sid, "threads")? {
                let thread = record_mut(value, sid, "thread")?;
                let local_id = required_string(thread, sid, "thread", "id")?
                    .parse::<u64>()
                    .map_err(|error| QueryProjectionError::MalformedRecord {
                        sid,
                        collection: "thread",
                        reason: format!("invalid id: {}", error),
                    })?;
                let global_id = thread_ids
                    .global_thread_id(sid, local_id)
                    .ok_or(QueryProjectionError::UnknownThread { sid, local_id })?;
                thread.insert("id".to_string(), global_id.to_string().into());
            }
            payload.insert(
                "current-thread-id".to_string(),
                current_thread_id.clone().into(),
            );
        }
        Ok(completion)
    }

    pub(crate) fn project_processes(
        &self,
        mut completion: FinishedCmd,
    ) -> Result<FinishedCmd, QueryProjectionError> {
        let thread_ids = self.state.read_thread_ids();
        for response in completion.get_responses_mut() {
            let sid = response.get_sid();
            let payload = response
                .get_payload_mut()
                .ok_or(DecodeError::MissingPayload { sid })?;
            for value in list_mut(payload, sid, "groups")? {
                let process = record_mut(value, sid, "process")?;
                let local_id = required_string(process, sid, "process", "id")?;
                required_string(process, sid, "process", "type")?;
                required_string(process, sid, "process", "pid")?;
                let global_id = thread_ids
                    .global_thread_group_id(sid, local_id)
                    .ok_or_else(|| QueryProjectionError::UnknownThreadGroup {
                        sid,
                        local_id: local_id.to_string(),
                    })?;
                process.insert("id".to_string(), global_id.to_string().into());
            }
        }
        Ok(completion)
    }
}

fn list_mut<'a>(
    payload: &'a mut Dict,
    sid: u64,
    field: &'static str,
) -> Result<&'a mut Vec<Value>, QueryProjectionError> {
    match payload.get_mut(field) {
        Some(Value::List(values)) => Ok(values),
        Some(_) => Err(DecodeError::UnexpectedType {
            sid,
            field,
            expected: "a list",
        }
        .into()),
        None => Err(DecodeError::MissingField { sid, field }.into()),
    }
}

fn record_mut<'a>(
    value: &'a mut Value,
    sid: u64,
    collection: &'static str,
) -> Result<&'a mut Dict, QueryProjectionError> {
    match value {
        Value::Dict(record) => Ok(record),
        _ => Err(QueryProjectionError::MalformedRecord {
            sid,
            collection,
            reason: "record must be a dictionary".to_string(),
        }),
    }
}

fn required_string<'a>(
    record: &'a Dict,
    sid: u64,
    collection: &'static str,
    field: &'static str,
) -> Result<&'a str, QueryProjectionError> {
    match record.get(field) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(QueryProjectionError::MalformedRecord {
            sid,
            collection,
            reason: format!("field '{}' must be a string", field),
        }),
        None => Err(QueryProjectionError::MalformedRecord {
            sid,
            collection,
            reason: format!("field '{}' is missing", field),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::cmd_flow::ParsedSessionResponse;

    fn completion(sid: u64, field: &str, records: Vec<Value>) -> FinishedCmd {
        FinishedCmd::new(
            Some(7),
            sid,
            vec![ParsedSessionResponse::new(
                sid,
                "done".to_string(),
                Some(Dict::from(HashMap::from([(
                    field.to_string(),
                    Value::List(records),
                )]))),
            )],
        )
    }

    #[tokio::test]
    async fn thread_projection_replaces_local_and_current_ids() {
        let state = StateMgr::new();
        state.add_thread_group(3, "i1").await;
        let (global_id, _) = state.create_thread(3, 9, "i1").await;
        state.select_thread_context(3, global_id);
        let input = completion(
            3,
            "threads",
            vec![Value::Dict(Dict::from(HashMap::from([(
                "id".to_string(),
                Value::from("9"),
            )])))],
        );

        let projected = QueryProjector::new(&state).project_threads(input).unwrap();
        let payload = projected.get_responses()[0].get_payload().unwrap();
        let thread = payload["threads"].expect_list_ref().unwrap()[0]
            .expect_dict_ref()
            .unwrap();
        assert_eq!(
            thread["id"].expect_string_ref().unwrap(),
            global_id.to_string()
        );
        assert_eq!(
            payload["current-thread-id"].expect_string_ref().unwrap(),
            global_id.to_string()
        );
    }

    #[tokio::test]
    async fn thread_projection_reuses_the_owned_record_list() {
        let state = StateMgr::new();
        state.add_thread_group(3, "i1").await;
        state.create_thread(3, 9, "i1").await;
        state.create_thread(3, 10, "i1").await;
        let input = completion(
            3,
            "threads",
            vec![
                Value::Dict(Dict::from(HashMap::from([(
                    "id".to_string(),
                    Value::from("9"),
                )]))),
                Value::Dict(Dict::from(HashMap::from([(
                    "id".to_string(),
                    Value::from("10"),
                )]))),
            ],
        );
        let records_before = input.get_responses()[0].get_payload().unwrap()["threads"]
            .expect_list_ref()
            .unwrap()
            .as_ptr();

        let projected = QueryProjector::new(&state).project_threads(input).unwrap();
        let records_after = projected.get_responses()[0].get_payload().unwrap()["threads"]
            .expect_list_ref()
            .unwrap()
            .as_ptr();

        assert_eq!(records_before, records_after);
    }

    #[tokio::test]
    async fn process_projection_replaces_local_group_ids() {
        let state = StateMgr::new();
        let global_id = state.add_thread_group(4, "i7").await;
        let process = Value::Dict(Dict::from(HashMap::from([
            ("id".to_string(), Value::from("i7")),
            ("type".to_string(), Value::from("process")),
            ("pid".to_string(), Value::from("42")),
        ])));

        let projected = QueryProjector::new(&state)
            .project_processes(completion(4, "groups", vec![process]))
            .unwrap();
        let process = projected.get_responses()[0].get_payload().unwrap()["groups"]
            .expect_list_ref()
            .unwrap()[0]
            .expect_dict_ref()
            .unwrap();
        assert_eq!(
            process["id"].expect_string_ref().unwrap(),
            global_id.to_string()
        );
    }

    #[test]
    fn unknown_thread_mapping_is_reported_without_panicking() {
        let state = StateMgr::new();
        let input = completion(
            8,
            "threads",
            vec![Value::Dict(Dict::from(HashMap::from([(
                "id".to_string(),
                Value::from("99"),
            )])))],
        );

        assert_eq!(
            QueryProjector::new(&state)
                .project_threads(input)
                .unwrap_err(),
            QueryProjectionError::UnknownThread {
                sid: 8,
                local_id: 99,
            }
        );
    }
}
