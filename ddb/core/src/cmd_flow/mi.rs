//! Compatibility presentation for DDB's established MI-shaped client wire.
//!
//! This formatter is deliberately independent of the debugger backend. GDB,
//! LLDB, and future backends all produce neutral protocol records before the
//! command layer renders client-facing output.

use crate::debugger::protocol::{Dict, List, Value};

pub(crate) struct MiFormatter;

impl MiFormatter {
    #[inline]
    fn escape_str(input: &str) -> String {
        input.chars().fold(String::new(), |mut output, character| {
            match character {
                '\\' => output.push_str("\\\\"),
                '"' => output.push_str("\\\""),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                other => output.push(other),
            }
            output
        })
    }

    #[inline]
    pub(crate) fn format_dict(payload: &Dict) -> String {
        payload
            .iter()
            .fold(String::new(), |acc, (key, value)| {
                let output = match value {
                    Value::String(value) => format!("\"{}\"", Self::escape_str(value)),
                    Value::List(values) => format!("[{}]", Self::format_list(values)),
                    Value::Dict(value) => format!("{{{}}}", Self::format_dict(value)),
                };
                format!("{acc},{key}={output}")
            })
            .trim_matches(',')
            .to_string()
    }

    #[inline]
    pub(crate) fn format_list(payload: &List) -> String {
        payload
            .iter()
            .fold(String::new(), |acc, value| {
                let output = match value {
                    Value::String(value) => format!("\"{value}\""),
                    Value::List(value) => format!("[{}]", Self::format_list(value)),
                    Value::Dict(value) => format!("{{{}}}", Self::format_dict(value)),
                };
                format!("{acc},{output}")
            })
            .trim_matches(',')
            .to_string()
    }

    #[inline]
    pub(crate) fn format(
        record_prefix: &str,
        message: &str,
        payload: Option<&Dict>,
        token: Option<u64>,
    ) -> String {
        let token = token.map(|token| token.to_string()).unwrap_or_default();
        let payload = payload
            .map(|payload| format!(",{}", Self::format_dict(payload)))
            .unwrap_or_default();

        format!("{token}{record_prefix}{message}{payload}")
    }

    pub(crate) fn format_stream(record_prefix: &str, message: &str) -> String {
        if record_prefix.is_empty() {
            message.to_string()
        } else {
            format!("{record_prefix}\"{}\"", Self::escape_str(message))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::debugger::gdb::parser::GdbParser;

    #[test]
    fn nested_values_round_trip_through_the_compatibility_wire() {
        let args = vec![
            Value::Dict(
                vec![
                    ("name".to_string(), Value::from("name")),
                    ("value".to_string(), Value::from("John")),
                ]
                .into(),
            ),
            Value::Dict(
                vec![
                    ("name".to_string(), Value::from("age")),
                    ("value".to_string(), Value::from("30")),
                ]
                .into(),
            ),
        ];
        let payload = Dict(
            vec![
                (
                    "reason".to_string(),
                    Value::from("there should be some reason"),
                ),
                (
                    "frame".to_string(),
                    Value::Dict(Dict(
                        vec![
                            ("addr".to_string(), Value::from("0x7f8d")),
                            ("func".to_string(), Value::from("say_hello")),
                            ("args".to_string(), Value::List(args)),
                        ]
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                    )),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let actual =
            GdbParser::parse(&MiFormatter::format("^", "stop", Some(&payload), None)).unwrap();
        let expected = GdbParser::parse(
            r#"^stop,reason="there should be some reason",frame={addr="0x7f8d",func="say_hello",args=[{name="name",value="John"},{name="age",value="30"}]}"#,
        )
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn debugger_streams_are_rendered_as_escaped_mi_c_strings() {
        assert_eq!(
            MiFormatter::format_stream("~", "line one\n\"quoted\"\\path"),
            "~\"line one\\n\\\"quoted\\\"\\\\path\""
        );
        assert_eq!(MiFormatter::format_stream("", "(gdb)"), "(gdb)");
    }
}
