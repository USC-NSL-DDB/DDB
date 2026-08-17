use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use gdbmi::{
    parser::{Message, Response},
    raw::GeneralMessage,
};

use crate::debugger::protocol::{DebuggerProtocol, ProtocolCommand, ProtocolRecord, StreamKind};

use super::parser::{normalize_dict, GdbParser};

/// GDB/MI command encoder and incremental output decoder.
#[derive(Debug, Default)]
pub struct GdbMiProtocol {
    buffer: BytesMut,
}

impl DebuggerProtocol for GdbMiProtocol {
    fn encode_command(&self, command: ProtocolCommand<'_>) -> Result<Bytes> {
        let tracked = if command.command.ends_with('\n') {
            format!("{}{}", command.token, command.command)
        } else {
            format!("{}{}\n", command.token, command.command)
        };
        let wire = match command.thread_id {
            Some(thread_id) => format!("-thread-select {}\n{}", thread_id, tracked),
            None => tracked,
        };
        Ok(Bytes::from(wire))
    }

    fn push_stdout(&mut self, bytes: Bytes) -> Result<Vec<ProtocolRecord>> {
        self.buffer.extend_from_slice(&bytes);
        let Some(last_newline) = self.buffer.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(Vec::new());
        };
        let complete = self.buffer.split_to(last_newline + 1);
        let text = std::str::from_utf8(&complete).context("GDB/MI output is not valid UTF-8")?;

        Ok(GdbParser::parse_multiple(text)
            .into_iter()
            .map(normalize_message)
            .collect())
    }
}

fn normalize_message(message: Message) -> ProtocolRecord {
    match message {
        Message::Response(Response::Notify {
            token,
            message,
            payload,
        }) => ProtocolRecord::Event {
            token: token.map(|token| token.0 as u64),
            message,
            payload: normalize_dict(payload),
        },
        Message::Response(Response::Result {
            token,
            message,
            payload,
        }) => ProtocolRecord::Result {
            token: token.map(|token| token.0 as u64),
            message,
            payload: payload.map(normalize_dict),
        },
        Message::General(message) => {
            let (kind, message) = match message {
                GeneralMessage::Console(message) => {
                    (StreamKind::Console, decode_mi_c_string(&message))
                }
                GeneralMessage::Log(message) => (StreamKind::Log, decode_mi_c_string(&message)),
                GeneralMessage::Target(message) => {
                    (StreamKind::Target, decode_mi_c_string(&message))
                }
                GeneralMessage::InferiorStdout(message) => (StreamKind::InferiorStdout, message),
                GeneralMessage::InferiorStderr(message) => (StreamKind::InferiorStderr, message),
                GeneralMessage::Done => (StreamKind::Prompt, String::new()),
            };
            ProtocolRecord::Stream { kind, message }
        }
    }
}

/// `gdbmi` deliberately preserves escapes in stream records. Normalize them
/// at the backend-neutral protocol boundary so HTTP clients receive the text
/// the debugger intended, while the compatibility publisher can escape it
/// exactly once on the way back to MI.
fn decode_mi_c_string(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' || index + 1 == bytes.len() {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        match bytes[index] {
            b'a' => decoded.push(0x07),
            b'b' => decoded.push(0x08),
            b'f' => decoded.push(0x0c),
            b'n' => decoded.push(b'\n'),
            b'r' => decoded.push(b'\r'),
            b't' => decoded.push(b'\t'),
            b'v' => decoded.push(0x0b),
            b'\\' => decoded.push(b'\\'),
            b'"' => decoded.push(b'"'),
            b'0'..=b'7' => {
                let mut value = bytes[index] - b'0';
                let mut digits = 1;
                while digits < 3
                    && index + 1 < bytes.len()
                    && matches!(bytes[index + 1], b'0'..=b'7')
                {
                    index += 1;
                    digits += 1;
                    value = value.saturating_mul(8).saturating_add(bytes[index] - b'0');
                }
                decoded.push(value);
            }
            other => decoded.push(other),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_encoding_owns_mi_token_and_thread_framing() {
        let protocol = GdbMiProtocol::default();
        assert_eq!(
            protocol
                .encode_command(ProtocolCommand {
                    token: 7,
                    command: "-thread-info",
                    thread_id: None,
                })
                .unwrap(),
            Bytes::from_static(b"7-thread-info\n")
        );
        assert_eq!(
            protocol
                .encode_command(ProtocolCommand {
                    token: 9,
                    command: "-stack-list-frames\n",
                    thread_id: Some(4),
                })
                .unwrap(),
            Bytes::from_static(b"-thread-select 4\n9-stack-list-frames\n")
        );
    }

    #[test]
    fn fragmented_and_coalesced_output_is_normalized() {
        let mut protocol = GdbMiProtocol::default();
        assert!(protocol
            .push_stdout(Bytes::from_static(b"2^done,value=\"sec"))
            .unwrap()
            .is_empty());

        let records = protocol
            .push_stdout(Bytes::from_static(
                b"ond\"\n=thread-created,id=\"3\",group-id=\"i1\"\n",
            ))
            .unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(
            &records[0],
            ProtocolRecord::Result {
                token: Some(2),
                payload: Some(payload),
                ..
            } if payload["value"].expect_string_ref().unwrap() == "second"
        ));
        assert!(matches!(
            &records[1],
            ProtocolRecord::Event {
                message,
                payload,
                ..
            } if message == "thread-created"
                && payload["group-id"].expect_string_ref().unwrap() == "i1"
        ));
    }

    #[test]
    fn stream_records_decode_gdb_c_escapes_once() {
        let mut protocol = GdbMiProtocol::default();
        let records = protocol
            .push_stdout(Bytes::from_static(
                b"~\"line one\\n\\\"quoted\\\"\\\\path\\011end\"\n",
            ))
            .unwrap();

        assert_eq!(
            records,
            vec![ProtocolRecord::Stream {
                kind: StreamKind::Console,
                message: "line one\n\"quoted\"\\path\tend".to_string(),
            }]
        );
    }
}
