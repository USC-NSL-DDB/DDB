use anyhow::{Context, Result};
use gdbmi::{
    self,
    raw::{Dict, List},
};
use tracing::error;

use gdbmi::parser::Message;

pub struct GdbParser;

impl GdbParser {
    #[inline]
    pub fn parse(output: &str) -> Result<Message> {
        let output = output.trim();
        Ok(gdbmi::parser::parse_message(output)
            .context(format!("Failed to parse a raw string: {}", output))?)
    }

    #[inline]
    pub fn parse_multiple(output: &str) -> Vec<Message> {
        output
            .trim()
            .split("\n")
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                if line == "(gdb)" {
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

pub struct MIFormatter;

impl MIFormatter {
    #[inline]
    pub fn format_dict(payload: &Dict) -> String {
        let payload = &payload.0;

        payload
            .iter()
            .fold(format!(""), |acc, (k, v)| {
                let out = match v {
                    gdbmi::raw::Value::String(s) => {
                        format!("\"{}\"", s)
                    }
                    gdbmi::raw::Value::List(l) => {
                        format!("[{}]", MIFormatter::format_list(l))
                    }
                    gdbmi::raw::Value::Dict(d) => {
                        format!("{{{}}}", MIFormatter::format_dict(d))
                    }
                };
                format!("{},{}={}", acc, k, out)
            })
            .trim_matches(',')
            .to_string()
    }

    #[inline]
    pub fn format_list(payload: &List) -> String {
        payload
            .iter()
            .fold(format!(""), |acc, v| {
                let out = match v {
                    gdbmi::raw::Value::String(s) => {
                        format!("\"{}\"", s)
                    }
                    gdbmi::raw::Value::List(l) => {
                        format!("[{}]", MIFormatter::format_list(l))
                    }
                    gdbmi::raw::Value::Dict(d) => {
                        format!("{{{}}}", MIFormatter::format_dict(d))
                    }
                };
                format!("{},{}", acc, out)
            })
            .trim_matches(',')
            .to_string()
    }

    #[inline]
    pub fn format(task_sym: &str, msg: &str, payload: Option<&Dict>, token: Option<u64>) -> String {
        let token = token.map(|t| format!("{}", t)).unwrap_or("".to_string());
        let payload = payload
            .map(|p| format!(",{}", MIFormatter::format_dict(p)))
            .unwrap_or_default();

        format!("{}{}{}{}", token, task_sym, msg, payload)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use gdbmi::{parser::*, raw::Value, Token};

    #[test]
    fn test_parse_imcomplete_result() {
        let output = r#"1^done,threads=[{id="1",target-id="LWP 17",name="server.out",frame={level="0",addr="0x000000000047b7a3",func="runtime.futex",args=[],file="/home/cc/go/pkg/mod/golang.org/toolchain@v0.0.1-go1.21.6.linux-amd64/src/runtime/sys_linux_amd64.s",fullname="/home/cc"#;
        let msg = GdbParser::parse_multiple(output.trim());
        println!("{:?}", msg);
    }

    #[test]
    fn test_parse_result() {
        let output = r#"1234^done"#;
        let msg = GdbParser::parse(output.trim()).unwrap();
        println!("{:?}", msg);
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

        match msg {
            Message::Response(Response::Notify {
                token,
                message,
                payload,
            }) => {
                assert_eq!(token, Some(Token(12345)));
                assert_eq!(message, "stopped");
                assert_eq!(payload.0.is_empty(), false);
            }
            _ => panic!("unexpected message type"),
        }
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

        match msg {
            Message::Response(Response::Notify {
                token,
                message,
                payload,
            }) => {
                assert_eq!(token, None);
                assert_eq!(message, "thread-created");
                // assert_eq!(payload.0., false);
                payload.as_map().iter().for_each(|(k, v)| {
                    if k == "id" {
                        assert_eq!(v.clone().expect_string().unwrap(), "3");
                    }
                    if k == "group-id" {
                        assert_eq!(v.clone().expect_string().unwrap(), "i1");
                    }
                });
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_parse_multiple() {
        let output = r#"1234^done
            12345*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",frame={addr="0x0000000000400b6c",func="main",args=[]},thread-id="1",stopped-threads="all"
            =thread-created,id="3",group-id="i1""
        "#;

        let msgs = GdbParser::parse_multiple(output);
        assert_eq!(msgs.len(), 3);

        assert!(matches!(
            msgs[0],
            Message::Response(Response::Result {
                token: _,
                message: _,
                payload: _
            })
        ));

        assert!(matches!(
            msgs[1],
            Message::Response(Response::Notify {
                token: _,
                message: _,
                payload: _
            })
        ));

        assert!(matches!(
            msgs[2],
            Message::Response(Response::Notify {
                token: _,
                message: _,
                payload: _
            })
        ));

        match &msgs[0] {
            Message::Response(Response::Result {
                token,
                message,
                payload,
            }) => {
                assert_eq!(*token, Some(Token(1234)));
                assert_eq!(message, "done");
                assert_eq!(*payload, None);
            }
            _ => panic!("unexpected message type"),
        }

        match &msgs[1] {
            Message::Response(Response::Notify {
                token,
                message,
                payload,
            }) => {
                assert_eq!(*token, Some(Token(12345)));
                assert_eq!(message, "stopped");
                assert_eq!(payload.0.is_empty(), false);
            }
            _ => panic!("unexpected message type"),
        }

        match &msgs[2] {
            Message::Response(Response::Notify {
                token,
                message,
                payload,
            }) => {
                assert_eq!(*token, None);
                assert_eq!(message, "thread-created");
                assert_eq!(payload.0.is_empty(), false);
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_mi_formatter_dict() {
        let args = vec![
            Value::Dict(Dict(
                vec![
                    ("name".to_string(), Value::String("name".to_string())),
                    ("value".to_string(), Value::String("John".to_string())),
                ]
                .into_iter()
                .collect::<HashMap<String, Value>>(),
            )),
            Value::Dict(Dict(
                vec![
                    ("name".to_string(), Value::String("age".to_string())),
                    ("value".to_string(), Value::String("30".to_string())),
                ]
                .into_iter()
                .collect::<HashMap<String, Value>>(),
            )),
        ];
        let inner_frame = Dict(
            vec![
                (
                    "addr".to_string(),
                    Value::String("0x00007f8d6f6b6b7f".to_string()),
                ),
                ("func".to_string(), Value::String("say_hello".to_string())),
                ("args".to_string(), Value::List(args)),
            ]
            .into_iter()
            .collect::<HashMap<String, Value>>(),
        );

        let payload = Dict(
            vec![
                (
                    "reason".to_string(),
                    Value::String("there should be some reason".to_string()),
                ),
                ("frame".to_string(), Value::Dict(inner_frame)),
                (
                    "stopped-threads".to_string(),
                    Value::List(vec![
                        Value::String("2".to_string()),
                        Value::String("3".to_string()),
                        Value::String("4".to_string()),
                    ]),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let output = MIFormatter::format("^", "stop", Some(&payload), None);

        let output = GdbParser::parse(&output).unwrap();
        let test_str = r#"^stop,reason="there should be some reason",frame={addr="0x00007f8d6f6b6b7f",func="say_hello",args=[{name="name",value="John"},{name="age",value="30"}]},stopped-threads=["2","3","4"]"#;
        let expected = GdbParser::parse(test_str).unwrap();

        assert_eq!(output, expected);
    }
}
