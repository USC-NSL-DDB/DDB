use anyhow::{Context, Result};
use gdbmi::{self};
use tracing::error;

use crate::debugger::protocol::{Dict, Value};
use gdbmi::parser::Message;

pub struct GdbParser;

impl GdbParser {
    #[inline]
    pub fn parse(output: &str) -> Result<Message> {
        let output = output.trim();
        gdbmi::parser::parse_message(output)
            .context(format!("Failed to parse a raw string: {}", output))
    }

    #[inline]
    pub fn parse_multiple(output: &str) -> Vec<Message> {
        output
            .trim()
            .split('\n')
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line == "(gdb)" {
                    return None;
                }
                match GdbParser::parse(line) {
                    Ok(msg) => Some(msg),
                    Err(e) => {
                        error!("Failed to parse a message: {}", e);
                        None
                    }
                }
            })
            .collect()
    }
}

pub(crate) fn normalize_dict(payload: gdbmi::raw::Dict) -> Dict {
    Dict(
        payload
            .0
            .into_iter()
            .map(|(key, value)| (key, normalize_value(value)))
            .collect(),
    )
}

fn normalize_value(value: gdbmi::raw::Value) -> Value {
    match value {
        gdbmi::raw::Value::String(value) => Value::String(value),
        gdbmi::raw::Value::List(values) => {
            Value::List(values.into_iter().map(normalize_value).collect())
        }
        gdbmi::raw::Value::Dict(value) => Value::Dict(normalize_dict(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdbmi::{parser::*, Token};

    #[test]
    #[should_panic]
    fn test_parse_imcomplete_result() {
        let output = r#"1^done,threads=[{id="1",target-id="LWP 17",name="server.out",frame={level="0",addr="0x000000000047b7a3",func="runtime.futex",args=[],file="/home/cc/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.21.6.linux-amd64/src/runtime/sys_linux_amd64.s",fullname="/home/cc"#;
        let _ = GdbParser::parse_multiple(output.trim());
    }

    #[test]
    fn test_parse_result() {
        let output = r#"1234^done"#;
        let msg = GdbParser::parse(output.trim()).unwrap();
        assert_eq!(
            msg,
            Message::Response(Response::Result {
                token: Some(Token(1234)),
                message: "done".to_string(),
                payload: None
            })
        );
    }

    #[test]
    fn test_parse_noti() {
        let output = r#"12345*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",frame={addr="0x0000000000400b6c",func="main",args=[]},thread-id="1",stopped-threads="all""#;
        let msg = GdbParser::parse(output.trim()).unwrap();
        assert!(matches!(
            msg,
            Message::Response(Response::Notify {
                token: _,
                message: _,
                payload: _
            })
        ));
    }

    #[test]
    fn test_parse_thread_creation() {
        let output = r#"=thread-created,id="3",group-id="i1""#;
        let msg = GdbParser::parse(output.trim()).unwrap();
        assert!(matches!(
            msg,
            Message::Response(Response::Notify {
                token: _,
                message: _,
                payload: _
            })
        ));
    }

    #[test]
    fn test_parse_multiple() {
        let output = r#"1234^done
            12345*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",frame={addr="0x0000000000400b6c",func="main",args=[]},thread-id="1",stopped-threads="all"
            =thread-created,id="3",group-id="i1""
        "#;

        let msgs = GdbParser::parse_multiple(output);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn test_mi_parse_inner_string_escape() {
        let output = r#"*stopped,reason="end-stepping-range",frame={addr="0x000075e25f7082a5",func="clusterInit",args=[{name="cluster_id",value="0x7ffedc6a85d0 \"cd6fab923d16922ebae3ade229880640\\300\\210j\\334\\376\\177\""}],file="/home/ybyan/proj/distributed-debugger/apps/redisraft/redisraft-bug-raft-c4de21/src/redisraft.c",fullname="/home/ybyan/proj/distributed-debugger/apps/redisraft/redisraft-bug-raft-c4de21/src/redisraft.c",line="1087",arch="i386:x86-64"},thread-id="1",stopped-threads="all",core="21""#;
        let msg = GdbParser::parse(output.trim()).unwrap();

        match msg.clone() {
            Message::Response(Response::Notify {
                token,
                message,
                payload,
            }) => {
                assert_eq!(token, None);
                assert_eq!(message, "stopped");
                assert!(!payload.0.is_empty());
                assert!(!normalize_dict(payload).is_empty());
            }
            _ => panic!("unexpected message type"),
        }
    }
}
