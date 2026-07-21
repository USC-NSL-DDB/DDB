//! Read-only debugger queries: dispatch and semantic projection.
//!
//! Query responses enter this module with debugger-local identifiers and leave
//! with DDB-global identifiers. Presenters can therefore remain pure and every
//! ingress observes the same response semantics.

use std::{fmt, sync::Arc};

use anyhow::{anyhow, Result};
use gdbmi::raw::{Dict, Value};

use super::{
    api::{self, CommandExecutor},
    decoder::DecodeError,
    input::ParsedInputCmd,
    router::Target,
    schema, CommandOutcome, FinishedCmd, Presentation,
};
use crate::state::{GlobalThreadId, StateMgr};

/// Owns the read-only query operations, symmetric to the other domain
/// services: the dispatcher classifies and delegates, this service resolves
/// targets, executes, and projects.
pub(crate) struct QueryService {
    executor: CommandExecutor,
    projector: QueryProjector,
}

impl QueryService {
    pub(crate) fn new(executor: CommandExecutor, projector: QueryProjector) -> Self {
        Self {
            executor,
            projector,
        }
    }

    pub(crate) async fn thread_info(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = match cmd.target {
            Target::Thread(global_tid) => {
                let (_, local_tid) = self.projector.resolve_thread(global_tid)?;
                let token = cmd
                    .external_token
                    .map(|token| token.to_string())
                    .unwrap_or_default();
                let command = format!("{token}-thread-info {local_tid}");
                self.executor
                    .execute_plan(api::command(&command)?.target(Target::Thread(global_tid)))
                    .await?
            }
            _ => self.executor.execute_plan(api::parsed(cmd)?).await?,
        };
        let response = self.projector.project_threads(response)?;
        Ok(CommandOutcome::response(response, Presentation::ThreadInfo))
    }

    pub(crate) async fn thread_select(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let parts = cmd.args.split_whitespace().collect::<Vec<_>>();
        let response = if let Some(global_tid) = parts.last() {
            let global_tid = global_tid.parse::<GlobalThreadId>()?;
            let (session_id, local_tid) = self.projector.resolve_thread(global_tid)?;
            let command = format!("-thread-select {local_tid}");
            self.executor
                .execute_plan(api::command(&command)?.target(Target::Session(session_id)))
                .await?
        } else {
            self.executor.execute_plan(api::parsed(cmd)?).await?
        };
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }

    pub(crate) async fn list_thread_groups(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = self
            .executor
            .execute_plan(api::parsed(cmd)?.target(Target::Broadcast))
            .await?;
        let response = self.projector.project_processes(response)?;
        Ok(CommandOutcome::response(
            response,
            Presentation::ProcessReadable,
        ))
    }

    pub(crate) async fn file_list_lines(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let session_id = self
            .projector
            .default_query_session()
            .ok_or_else(|| anyhow!("no debugger session is available to list source lines"))?;
        let response = self
            .executor
            .execute_plan(api::parsed(cmd)?.target(Target::Session(session_id)))
            .await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }
}

impl fmt::Debug for QueryService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("QueryService").finish()
    }
}

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
    UnknownGlobalThread { global_id: GlobalThreadId },
    #[error("session {sid} thread group '{local_id}' has no global group mapping")]
    UnknownThreadGroup { sid: u64, local_id: String },
}

#[derive(Clone)]
pub(crate) struct QueryProjector {
    state: Arc<StateMgr>,
}

impl fmt::Debug for QueryProjector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("QueryProjector").finish()
    }
}

impl QueryProjector {
    pub(crate) fn new(state: Arc<StateMgr>) -> Self {
        Self { state }
    }

    pub(crate) fn resolve_thread(
        &self,
        global_id: GlobalThreadId,
    ) -> Result<(u64, u64), QueryProjectionError> {
        self.state
            .local_thread_id(global_id)
            .map(|local| local.into_parts())
            .ok_or(QueryProjectionError::UnknownGlobalThread { global_id })
    }

    /// The session queries fall back to when a command has no usable target:
    /// the selected session, else the lowest live session id.
    pub(crate) fn default_query_session(&self) -> Option<u64> {
        self.state
            .current_session_id()
            .or_else(|| self.state.session_ids().into_iter().min())
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
            for value in list_mut(payload, sid, schema::THREADS)? {
                let thread = record_mut(value, sid, "thread")?;
                let local_id = required_string(thread, sid, "thread", schema::RECORD_ID)?
                    .parse::<u64>()
                    .map_err(|error| QueryProjectionError::MalformedRecord {
                        sid,
                        collection: "thread",
                        reason: format!("invalid id: {}", error),
                    })?;
                let global_id = thread_ids
                    .global_thread_id(sid, local_id)
                    .ok_or(QueryProjectionError::UnknownThread { sid, local_id })?;
                thread.insert(schema::RECORD_ID.to_string(), global_id.to_string().into());
            }
            payload.insert(
                schema::CURRENT_THREAD_ID.to_string(),
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
            for value in list_mut(payload, sid, schema::GROUPS)? {
                let process = record_mut(value, sid, "process")?;
                let local_id = required_string(process, sid, "process", schema::RECORD_ID)?;
                required_string(process, sid, "process", schema::PROCESS_TYPE)?;
                required_string(process, sid, "process", schema::PROCESS_PID)?;
                let global_id = thread_ids
                    .global_thread_group_id(sid, local_id)
                    .ok_or_else(|| QueryProjectionError::UnknownThreadGroup {
                        sid,
                        local_id: local_id.to_string(),
                    })?;
                process.insert(schema::RECORD_ID.to_string(), global_id.to_string().into());
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
        let state = Arc::new(StateMgr::new());
        state.register_session(3, "svc", None).await;
        state.register_thread_group(3, "i1").await.unwrap();
        let global_id = state.register_thread(3, 9, "i1").await.unwrap().thread_id;
        state.select_thread_context(3, global_id);
        let input = completion(
            3,
            "threads",
            vec![Value::Dict(Dict::from(HashMap::from([(
                "id".to_string(),
                Value::from("9"),
            )])))],
        );

        let projected = QueryProjector::new(Arc::clone(&state))
            .project_threads(input)
            .unwrap();
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
        let state = Arc::new(StateMgr::new());
        state.register_session(3, "svc", None).await;
        state.register_thread_group(3, "i1").await.unwrap();
        state.register_thread(3, 9, "i1").await.unwrap();
        state.register_thread(3, 10, "i1").await.unwrap();
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

        let projected = QueryProjector::new(Arc::clone(&state))
            .project_threads(input)
            .unwrap();
        let records_after = projected.get_responses()[0].get_payload().unwrap()["threads"]
            .expect_list_ref()
            .unwrap()
            .as_ptr();

        assert_eq!(records_before, records_after);
    }

    #[tokio::test]
    async fn process_projection_replaces_local_group_ids() {
        let state = Arc::new(StateMgr::new());
        state.register_session(4, "svc", None).await;
        let global_id = state.register_thread_group(4, "i7").await.unwrap();
        let process = Value::Dict(Dict::from(HashMap::from([
            ("id".to_string(), Value::from("i7")),
            ("type".to_string(), Value::from("process")),
            ("pid".to_string(), Value::from("42")),
        ])));

        let projected = QueryProjector::new(Arc::clone(&state))
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
        let state = Arc::new(StateMgr::new());
        let input = completion(
            8,
            "threads",
            vec![Value::Dict(Dict::from(HashMap::from([(
                "id".to_string(),
                Value::from("99"),
            )])))],
        );

        assert_eq!(
            QueryProjector::new(Arc::clone(&state))
                .project_threads(input)
                .unwrap_err(),
            QueryProjectionError::UnknownThread {
                sid: 8,
                local_id: 99,
            }
        );
    }
}
