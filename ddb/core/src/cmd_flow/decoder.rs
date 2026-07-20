//! Typed decoding for debugger command completions.
//!
//! The debugger protocol remains dictionary-shaped at the transport boundary,
//! but application services should not know field names or panic on malformed
//! responses. This module is the single checked boundary between those layers.

use std::fmt::Display;

use gdbmi::raw::{Dict, Value};

use super::{FinishedCmd, ParsedSessionResponse};

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum DecodeError {
    #[error("debugger returned no session response")]
    MissingResponse,
    #[error("debugger response for session {sid} has no payload")]
    MissingPayload { sid: u64 },
    #[error("debugger response for session {sid} is missing field '{field}'")]
    MissingField { sid: u64, field: &'static str },
    #[error("debugger field '{field}' for session {sid} must be {expected}")]
    UnexpectedType {
        sid: u64,
        field: &'static str,
        expected: &'static str,
    },
    #[error("invalid debugger field '{field}' for session {sid}: {reason}")]
    InvalidValue {
        sid: u64,
        field: &'static str,
        reason: String,
    },
    #[error("debugger operation '{operation}' failed for session {sid}: {message}")]
    OperationFailed {
        sid: u64,
        operation: String,
        message: String,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Payload<'a> {
    sid: u64,
    value: &'a Dict,
}

impl<'a> Payload<'a> {
    pub(crate) fn from_response(response: &'a ParsedSessionResponse) -> Result<Self, DecodeError> {
        let sid = response.get_sid();
        let value = response
            .get_payload()
            .ok_or(DecodeError::MissingPayload { sid })?;
        Ok(Self { sid, value })
    }

    pub(crate) fn first(completion: &'a FinishedCmd) -> Result<Self, DecodeError> {
        let response = completion
            .get_responses()
            .first()
            .ok_or(DecodeError::MissingResponse)?;
        Self::from_response(response)
    }

    pub(crate) fn sid(self) -> u64 {
        self.sid
    }

    pub(crate) fn value(self, field: &'static str) -> Result<&'a Value, DecodeError> {
        self.value.get(field).ok_or(DecodeError::MissingField {
            sid: self.sid,
            field,
        })
    }

    pub(crate) fn string(self, field: &'static str) -> Result<&'a str, DecodeError> {
        match self.value(field)? {
            Value::String(value) => Ok(value),
            _ => Err(DecodeError::UnexpectedType {
                sid: self.sid,
                field,
                expected: "a string",
            }),
        }
    }

    pub(crate) fn list(self, field: &'static str) -> Result<&'a [Value], DecodeError> {
        match self.value(field)? {
            Value::List(value) => Ok(value),
            _ => Err(DecodeError::UnexpectedType {
                sid: self.sid,
                field,
                expected: "a list",
            }),
        }
    }

    pub(crate) fn optional_string(
        self,
        field: &'static str,
    ) -> Result<Option<&'a str>, DecodeError> {
        match self.value.get(field) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(DecodeError::UnexpectedType {
                sid: self.sid,
                field,
                expected: "a string",
            }),
        }
    }

    pub(crate) fn parse<T>(self, field: &'static str) -> Result<T, DecodeError>
    where
        T: std::str::FromStr,
        T::Err: Display,
    {
        self.string(field)?
            .parse::<T>()
            .map_err(|error| DecodeError::InvalidValue {
                sid: self.sid,
                field,
                reason: error.to_string(),
            })
    }

    pub(crate) fn nested_dict(self, field: &'static str) -> Result<Payload<'a>, DecodeError> {
        match self.value(field)? {
            Value::Dict(value) => Ok(Payload {
                sid: self.sid,
                value,
            }),
            _ => Err(DecodeError::UnexpectedType {
                sid: self.sid,
                field,
                expected: "a dictionary",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct BreakpointCreated {
    pub(crate) local_id: u64,
    pub(crate) times: u64,
}

impl BreakpointCreated {
    pub(crate) fn decode(response: &ParsedSessionResponse) -> Result<Self, DecodeError> {
        let breakpoint = Payload::from_response(response)?.nested_dict("bkpt")?;
        Ok(Self {
            local_id: breakpoint.parse("number")?,
            times: breakpoint.parse("times")?,
        })
    }

    pub(crate) fn decode_first(completion: &FinishedCmd) -> Result<Self, DecodeError> {
        let response = completion
            .get_responses()
            .first()
            .ok_or(DecodeError::MissingResponse)?;
        Self::decode(response)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct OperationStatus {
    pub(crate) sid: u64,
    pub(crate) success: bool,
    pub(crate) message: String,
}

impl OperationStatus {
    pub(crate) fn decode(completion: &FinishedCmd) -> Result<Self, DecodeError> {
        let payload = Payload::first(completion)?;
        Ok(Self {
            sid: payload.sid(),
            success: payload.parse("success")?,
            message: payload
                .optional_string("message")?
                .unwrap_or_default()
                .to_string(),
        })
    }

    pub(crate) fn require_success(self, operation: impl Into<String>) -> Result<Self, DecodeError> {
        if self.success {
            Ok(self)
        } else {
            Err(DecodeError::OperationFailed {
                sid: self.sid,
                operation: operation.into(),
                message: self.message,
            })
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ProcletLocality {
    pub(crate) is_local: bool,
}

impl ProcletLocality {
    pub(crate) fn decode(completion: &FinishedCmd) -> Result<Self, DecodeError> {
        OperationStatus::decode(completion)?.require_success("check-proclet")?;
        let payload = Payload::first(completion)?;
        Ok(Self {
            is_local: payload.parse("is_local")?,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ProcletHeap {
    sid: u64,
    pub(crate) start_addr: u64,
    pub(crate) end_addr: u64,
    pub(crate) data_len: u64,
    pub(crate) data: String,
    pub(crate) full_heap_size: u64,
}

impl ProcletHeap {
    pub(crate) fn decode(completion: &FinishedCmd) -> Result<Self, DecodeError> {
        OperationStatus::decode(completion)?.require_success("get-proclet-heap")?;
        let payload = Payload::first(completion)?;
        Ok(Self {
            sid: payload.sid(),
            start_addr: payload.parse("start")?,
            end_addr: payload.parse("end")?,
            data_len: payload.parse("len")?,
            data: payload.string("heap_content")?.to_string(),
            full_heap_size: payload.parse("full_heap_size")?,
        })
    }

    pub(crate) fn validate(self) -> Result<Self, DecodeError> {
        if self.start_addr == 0
            || (self.end_addr == 0 && self.data_len == 0)
            || self.full_heap_size == 0
        {
            return Err(DecodeError::InvalidValue {
                sid: self.sid,
                field: "proclet heap",
                reason: "heap addresses and size must be non-zero".to_string(),
            });
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn completion(sid: u64, payload: Option<Dict>) -> FinishedCmd {
        FinishedCmd::new(
            None,
            sid,
            vec![ParsedSessionResponse::new(sid, "done".to_string(), payload)],
        )
    }

    #[test]
    fn breakpoint_result_is_decoded_into_numbers() {
        let payload: Dict = HashMap::from([(
            "bkpt".to_string(),
            Dict::from(HashMap::from([
                ("number".to_string(), Value::from("17")),
                ("times".to_string(), Value::from("3")),
            ]))
            .into(),
        )])
        .into();

        assert_eq!(
            BreakpointCreated::decode_first(&completion(4, Some(payload))).unwrap(),
            BreakpointCreated {
                local_id: 17,
                times: 3,
            }
        );
    }

    #[test]
    fn missing_payload_reports_the_originating_session() {
        assert_eq!(
            Payload::first(&completion(9, None)).unwrap_err(),
            DecodeError::MissingPayload { sid: 9 }
        );
    }

    #[test]
    fn invalid_scalar_is_an_error_instead_of_a_panic() {
        let payload: Dict = HashMap::from([(
            "bkpt".to_string(),
            Dict::from(HashMap::from([
                ("number".to_string(), Value::from("not-a-number")),
                ("times".to_string(), Value::from("0")),
            ]))
            .into(),
        )])
        .into();

        assert!(matches!(
            BreakpointCreated::decode_first(&completion(2, Some(payload))),
            Err(DecodeError::InvalidValue {
                sid: 2,
                field: "number",
                ..
            })
        ));
    }

    #[test]
    fn failed_operations_preserve_the_debugger_message() {
        let payload: Dict = HashMap::from([
            ("success".to_string(), Value::from("false")),
            ("message".to_string(), Value::from("heap unavailable")),
        ])
        .into();

        let error = OperationStatus::decode(&completion(12, Some(payload)))
            .unwrap()
            .require_success("restore-proclet-heap")
            .unwrap_err();
        assert_eq!(
            error,
            DecodeError::OperationFailed {
                sid: 12,
                operation: "restore-proclet-heap".to_string(),
                message: "heap unavailable".to_string(),
            }
        );
    }
}
