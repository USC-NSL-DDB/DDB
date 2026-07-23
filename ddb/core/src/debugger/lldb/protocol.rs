use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};

use crate::debugger::protocol::{
    DebuggerProtocol, Dict, ProtocolCommand, ProtocolRecord, StreamKind, Value,
};

const RECORD_PREFIX: &str = "@DDB@";
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct LldbJsonProtocol {
    buffer: BytesMut,
}

#[derive(Serialize)]
struct Request<'a> {
    id: u64,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Response {
    Event {
        #[serde(default)]
        token: Option<u64>,
        message: String,
        #[serde(default)]
        payload: serde_json::Map<String, serde_json::Value>,
    },
    Result {
        #[serde(default)]
        token: Option<u64>,
        message: String,
        #[serde(default)]
        payload: Option<serde_json::Map<String, serde_json::Value>>,
    },
    Stream {
        #[serde(default)]
        stream: JsonStreamKind,
        message: String,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum JsonStreamKind {
    #[default]
    Console,
    Log,
    Target,
    InferiorStdout,
    InferiorStderr,
}

impl DebuggerProtocol for LldbJsonProtocol {
    fn encode_command(&self, command: ProtocolCommand<'_>) -> Result<Bytes> {
        encode_request(command.token, command.command, command.thread_id)
    }

    fn push_stdout(&mut self, bytes: Bytes) -> Result<Vec<ProtocolRecord>> {
        self.buffer.extend_from_slice(&bytes);
        if self.buffer.len() > MAX_RECORD_BYTES && !self.buffer.contains(&b'\n') {
            bail!(
                "LLDB bridge record exceeded {} bytes without a delimiter",
                MAX_RECORD_BYTES
            );
        }

        let Some(last_newline) = self.buffer.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(Vec::new());
        };
        let complete = self.buffer.split_to(last_newline + 1);
        let text =
            std::str::from_utf8(&complete).context("LLDB bridge output is not valid UTF-8")?;
        text.lines()
            .filter_map(decode_line)
            .collect::<Result<Vec<_>>>()
    }
}

pub(crate) fn encode_request(token: u64, command: &str, thread_id: Option<u64>) -> Result<Bytes> {
    let mut encoded = serde_json::to_vec(&Request {
        id: token,
        command,
        thread_id,
    })
    .context("failed to encode LLDB bridge request")?;
    encoded.push(b'\n');
    Ok(Bytes::from(encoded))
}

fn decode_line(line: &str) -> Option<Result<ProtocolRecord>> {
    let Some(prefix) = line.find(RECORD_PREFIX) else {
        let line = line.trim();
        return (!line.is_empty()).then(|| {
            Ok(ProtocolRecord::Stream {
                kind: StreamKind::Console,
                message: line.to_string(),
            })
        });
    };
    let json = &line[prefix + RECORD_PREFIX.len()..];
    Some(
        serde_json::from_str::<Response>(json)
            .with_context(|| format!("failed to decode LLDB bridge record: {json}"))
            .and_then(normalize_response),
    )
}

fn normalize_response(response: Response) -> Result<ProtocolRecord> {
    match response {
        Response::Event {
            token,
            message,
            payload,
        } => Ok(ProtocolRecord::Event {
            token,
            message,
            payload: normalize_dict(payload)?,
        }),
        Response::Result {
            token,
            message,
            payload,
        } => Ok(ProtocolRecord::Result {
            token,
            message,
            payload: payload.map(normalize_dict).transpose()?,
        }),
        Response::Stream { stream, message } => Ok(ProtocolRecord::Stream {
            kind: match stream {
                JsonStreamKind::Console => StreamKind::Console,
                JsonStreamKind::Log => StreamKind::Log,
                JsonStreamKind::Target => StreamKind::Target,
                JsonStreamKind::InferiorStdout => StreamKind::InferiorStdout,
                JsonStreamKind::InferiorStderr => StreamKind::InferiorStderr,
            },
            message,
        }),
    }
}

fn normalize_dict(values: serde_json::Map<String, serde_json::Value>) -> Result<Dict> {
    values
        .into_iter()
        .map(|(key, value)| normalize_value(value).map(|value| (key, value)))
        .collect::<Result<HashMap<_, _>>>()
        .map(Dict)
}

fn normalize_value(value: serde_json::Value) -> Result<Value> {
    match value {
        serde_json::Value::String(value) => Ok(Value::String(value)),
        serde_json::Value::Number(value) => Ok(Value::String(value.to_string())),
        serde_json::Value::Bool(value) => Ok(Value::String(value.to_string())),
        serde_json::Value::Null => Ok(Value::String(String::new())),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(normalize_value)
            .collect::<Result<Vec<_>>>()
            .map(Value::List),
        serde_json::Value::Object(values) => normalize_dict(values).map(Value::Dict),
    }
    .map_err(|error| anyhow!(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_json_framed_without_mi_token_rules() {
        let protocol = LldbJsonProtocol::default();
        let wire = protocol
            .encode_command(ProtocolCommand {
                token: 9,
                command: "-stack-list-frames",
                thread_id: Some(4),
            })
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&wire).unwrap(),
            "{\"id\":9,\"command\":\"-stack-list-frames\",\"thread_id\":4}\n"
        );
    }

    #[test]
    fn fragmented_json_records_normalize_scalar_types() {
        let mut protocol = LldbJsonProtocol::default();
        assert!(protocol
            .push_stdout(Bytes::from_static(
                b"@DDB@{\"type\":\"result\",\"token\":2,"
            ))
            .unwrap()
            .is_empty());
        let records = protocol
            .push_stdout(Bytes::from_static(
                b"\"message\":\"done\",\"payload\":{\"pid\":42,\"ok\":true}}\n",
            ))
            .unwrap();
        assert!(matches!(
            &records[0],
            ProtocolRecord::Result {
                token: Some(2),
                payload: Some(payload),
                ..
            } if payload["pid"].expect_string_ref().unwrap() == "42"
                && payload["ok"].expect_string_ref().unwrap() == "true"
        ));
    }

    #[test]
    fn native_lldb_noise_is_a_stream_record_not_a_protocol_fault() {
        let mut protocol = LldbJsonProtocol::default();
        let records = protocol
            .push_stdout(Bytes::from_static(
                b"(lldb) command script import bridge.py\n",
            ))
            .unwrap();
        assert_eq!(
            records,
            vec![ProtocolRecord::Stream {
                kind: StreamKind::Console,
                message: "(lldb) command script import bridge.py".to_string(),
            }]
        );
    }
}
