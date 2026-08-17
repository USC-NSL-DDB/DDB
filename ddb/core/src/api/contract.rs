//! Stable, versioned HTTP contracts for DDB clients.
//!
//! The debugger protocol deliberately keeps its historical tagged value
//! representation. The HTTP API translates that internal representation to
//! ordinary JSON so clients do not need to know which debugger backend or wire
//! protocol produced a value.

use std::collections::HashSet;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use uuid::Uuid;

use crate::{
    cmd_flow::{router::Target, FinishedCmd},
    debugger::protocol::Value,
    state::{GlobalThreadId, GroupId},
};

pub const API_VERSION: &str = "v1";

#[derive(Debug, Serialize)]
pub struct Success<T> {
    pub api_version: &'static str,
    pub request_id: Uuid,
    pub data: T,
}

impl<T> Success<T> {
    pub fn new(data: T) -> Self {
        Self {
            api_version: API_VERSION,
            request_id: Uuid::new_v4(),
            data,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub api_version: &'static str,
    pub request_id: Uuid,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Option<JsonValue>,
}

impl ApiError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    pub fn internal(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }

    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: JsonValue) -> Self {
        self.details = Some(details);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                api_version: API_VERSION,
                request_id: Uuid::new_v4(),
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    details: self.details,
                },
            }),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<Json<Success<T>>, ApiError>;

pub fn success<T>(data: T) -> Json<Success<T>> {
    Json(Success::new(data))
}

/// Stable API target syntax. This avoids exposing serde's representation of
/// DDB's internal routing enum and leaves room for target-specific metadata.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiTarget {
    Session { session_id: u64 },
    Thread { thread_id: u64 },
    Group { group_id: u64 },
    CurrentThread,
    CurrentSession,
    SessionSet { session_ids: Vec<u64> },
    Broadcast,
    First,
    Multiple { targets: Vec<ApiTarget> },
}

impl TryFrom<ApiTarget> for Target {
    type Error = ApiError;

    fn try_from(value: ApiTarget) -> Result<Self, Self::Error> {
        Ok(match value {
            ApiTarget::Session { session_id } => Self::Session(session_id),
            ApiTarget::Thread { thread_id } => Self::Thread(GlobalThreadId::new(thread_id)),
            ApiTarget::Group { group_id } => Self::Group(GroupId::new(group_id)),
            ApiTarget::CurrentThread => Self::CurrThread,
            ApiTarget::CurrentSession => Self::CurrSession,
            ApiTarget::SessionSet { session_ids } => {
                if session_ids.is_empty() {
                    return Err(ApiError::bad_request(
                        "empty_target",
                        "session_ids must contain at least one session",
                    ));
                }
                Self::SessionSet(session_ids.into_iter().collect::<HashSet<_>>())
            }
            ApiTarget::Broadcast => Self::Broadcast,
            ApiTarget::First => Self::First,
            ApiTarget::Multiple { targets } => {
                if targets.is_empty() {
                    return Err(ApiError::bad_request(
                        "empty_target",
                        "targets must contain at least one target",
                    ));
                }
                Self::Multiple(
                    targets
                        .into_iter()
                        .map(Target::try_from)
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
        })
    }
}

fn default_wait() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
pub struct CommandRequest {
    #[serde(alias = "cmd")]
    pub command: String,
    #[serde(default)]
    pub target: Option<ApiTarget>,
    #[serde(default = "default_wait")]
    pub wait: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandReceipt {
    pub state: CommandReceiptState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<CommandCompletion>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandReceiptState {
    Accepted,
    Completed,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandCompletion {
    pub external_token: Option<u64>,
    pub primary_session_id: Option<u64>,
    pub responses: Vec<SessionCommandResponse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionCommandResponse {
    pub session_id: u64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<JsonValue>,
}

impl From<&FinishedCmd> for CommandCompletion {
    fn from(value: &FinishedCmd) -> Self {
        Self {
            external_token: value.get_external_token(),
            primary_session_id: (value.get_sid() != 0).then_some(value.get_sid()),
            responses: value
                .get_responses()
                .iter()
                .map(|response| SessionCommandResponse {
                    session_id: response.get_sid(),
                    status: response.get_message().clone(),
                    payload: response.get_payload().map(dict_to_json),
                })
                .collect(),
        }
    }
}

pub fn dict_to_json(value: &crate::debugger::protocol::Dict) -> JsonValue {
    JsonValue::Object(
        value
            .as_map()
            .iter()
            .map(|(key, value)| (key.clone(), protocol_value_to_json(value)))
            .collect::<Map<_, _>>(),
    )
}

pub fn protocol_value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::String(value) => JsonValue::String(value.clone()),
        Value::List(values) => {
            JsonValue::Array(values.iter().map(protocol_value_to_json).collect())
        }
        Value::Dict(value) => dict_to_json(value),
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAction {
    Continue,
    Interrupt,
    Next,
    StepIn,
    StepOut,
    Jump,
    SendSignal,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecutionRequest {
    pub action: ExecutionAction,
    pub target: ApiTarget,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub signal: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ThreadQueryRequest {
    #[serde(default)]
    pub target: Option<ApiTarget>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ThreadSelectRequest {
    pub thread_id: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StackFramesRequest {
    pub thread_id: u64,
    #[serde(default)]
    pub low: Option<u64>,
    #[serde(default)]
    pub high: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableValues {
    None,
    #[default]
    Simple,
    All,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StackVariablesRequest {
    pub thread_id: u64,
    #[serde(default)]
    pub frame: Option<u64>,
    #[serde(default)]
    pub values: VariableValues,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EvaluateRequest {
    pub expression: String,
    pub target: ApiTarget,
    #[serde(default)]
    pub frame: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MemoryReadRequest {
    pub address: String,
    pub count: u64,
    pub target: ApiTarget,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BreakpointCreateRequest {
    pub source: String,
    pub line: u64,
    pub target: ApiTarget,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub temporary: bool,
    #[serde(default)]
    pub hardware: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BreakpointUpdateRequest {
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DistributedBacktraceRequest {
    pub thread_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cmd_flow::ParsedSessionResponse,
        debugger::protocol::{Dict, Value},
    };

    #[test]
    fn target_wire_schema_is_stable_and_human_readable() {
        let target: ApiTarget = serde_json::from_value(serde_json::json!({
            "kind": "multiple",
            "targets": [
                {"kind": "session", "session_id": 7},
                {"kind": "group", "group_id": 3}
            ]
        }))
        .unwrap();

        assert_eq!(
            Target::try_from(target).unwrap(),
            Target::Multiple(vec![Target::Session(7), Target::Group(GroupId::new(3))])
        );
    }

    #[test]
    fn command_completions_use_ordinary_json_values() {
        let payload: Dict = vec![(
            "threads".to_string(),
            Value::List(vec![Value::Dict(
                vec![("id".to_string(), Value::from("41"))].into(),
            )]),
        )]
        .into();
        let completion = FinishedCmd::new(
            Some(9),
            7,
            vec![ParsedSessionResponse::new(
                7,
                "done".to_string(),
                Some(payload),
            )],
        );

        let value = serde_json::to_value(CommandCompletion::from(&completion)).unwrap();
        assert_eq!(value["responses"][0]["payload"]["threads"][0]["id"], "41");
        assert!(value["responses"][0]["payload"]["threads"][0]["id"]
            .get("String")
            .is_none());
    }
}
