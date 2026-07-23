use std::collections::HashMap;

use crate::debugger::protocol::{Dict, Value};

use crate::debugger::gdb::parser::MIFormatter;

use super::{schema, FinishedCmd};

/// Formatter trait for transforming and formatting GDB responses
///
/// # Formatter Responsibilities
///
/// 1. **Transform**: Convert raw `FinishedCmd` responses into a structured representation
///    - May swap local thread IDs with global thread IDs
///    - May enrich responses with additional context
///    - May filter or aggregate responses
///
/// 2. **Format**: Convert the transformed representation into output strings
///    - Plain text for CLI
///    - JSON for IDE integrations
///    - Human-readable formats for debugging
///
/// # Default Implementations
///
/// - **`PlainFormatter`** (default): Returns first response in MI format, no transformation
/// - **`ThreadInfoFormatter`**: Swaps local thread IDs → global IDs, formats as JSON
/// - **`UnitFormatter`**: Returns all responses in original form (multi-response)
///
/// # Example
///
/// ```no_run
/// # use super::{Formatter, FinishedCmd};
/// struct MyFormatter;
/// impl Formatter for MyFormatter {
///     type Transformed = String;
///     
///     fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
///         // Extract relevant data
///         responses.get_responses().first()
///             .map(|r| r.get_message().clone())
///             .unwrap_or_default()
///     }
///     
///     fn format(&self, input: &Self::Transformed) -> String {
///         format!("Result: {}", input)
///     }
/// }
/// ```
pub trait Formatter: Clone {
    type Transformed;

    // transform the responses, e.g. swap thread id with our own tracked global id
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed;
    // format the responses into a string. (ready to be printed)
    fn format(&self, input: &Self::Transformed) -> String;
}

#[derive(Clone)]
pub struct PlainFormatter;
impl Formatter for PlainFormatter {
    type Transformed = FinishedCmd;

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        responses
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        let r = input.get_responses().first().unwrap();
        MIFormatter::format(
            "^",
            r.get_message(),
            r.get_payload(),
            input.get_external_token(),
        )
    }
}

/// UnitFormatter outputs all responses in their original form.
#[derive(Clone)]
pub struct UnitFormatter;
impl Formatter for UnitFormatter {
    type Transformed = FinishedCmd;

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        responses
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        let formatted = input
            .get_responses()
            .iter()
            .map(|r| {
                MIFormatter::format(
                    "^",
                    r.get_message(),
                    r.get_payload(),
                    input.get_external_token(),
                )
            })
            .collect::<Vec<_>>();
        formatted.join("\n")
    }
}

/// handle `-thread-info` command response
#[derive(Clone)]
pub struct ThreadInfoFormatter;
impl Formatter for ThreadInfoFormatter {
    // (token, transformed responses)
    type Transformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        let mut all_thread_info = Vec::<Value>::new();
        let mut current_thread_id = String::new();

        for resp in responses.get_responses() {
            if let Some(payload) = resp.get_payload() {
                if current_thread_id.is_empty() {
                    if let Some(Value::String(id)) = payload.get(schema::CURRENT_THREAD_ID) {
                        current_thread_id = id.clone();
                    }
                }
                if let Some(Value::List(threads)) = payload.get(schema::THREADS) {
                    all_thread_info.extend(threads.iter().cloned());
                }
            }
        }

        (
            responses.get_external_token(),
            vec![
                (schema::THREADS.to_string(), all_thread_info.into()),
                (
                    schema::CURRENT_THREAD_ID.to_string(),
                    current_thread_id.into(),
                ),
            ]
            .into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        format_done(input.0, &input.1)
    }
}

/// Aggregates per-session `-list-thread-groups` payloads into one record list.
fn aggregate_process_groups(responses: &FinishedCmd) -> Vec<Value> {
    let mut all_process_info = Vec::<Value>::new();
    for resp in responses.get_responses() {
        if let Some(payload) = resp.get_payload() {
            if let Some(Value::List(processes)) = payload.get(schema::GROUPS) {
                all_process_info.extend(processes.iter().cloned());
            }
        }
    }
    all_process_info
}

/// handle `info inferiors` command response
#[derive(Clone)]
pub struct ProcessReadableFormatter;
impl Formatter for ProcessReadableFormatter {
    type Transformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        let processes = aggregate_process_groups(&responses);
        let readable_pinfo: Vec<Value> = processes
            .iter()
            .map(|p| {
                let p = p.expect_dict_ref().unwrap();
                let id = p[schema::RECORD_ID]
                    .expect_string_ref()
                    .unwrap()
                    .to_string();
                let ptype = p[schema::PROCESS_TYPE].expect_string_ref().unwrap();
                let pid = p[schema::PROCESS_PID].expect_string_ref().unwrap();
                let exec = p
                    .get(schema::PROCESS_EXECUTABLE)
                    .unwrap_or(&Value::String("".to_string()))
                    .clone();

                Value::Dict(
                    vec![
                        (schema::RECORD_ID.to_string(), id.into()),
                        (
                            schema::PROCESS_DESC.to_string(),
                            format!("{} {}", ptype, pid).into(),
                        ),
                        (schema::PROCESS_EXECUTABLE.to_string(), exec),
                    ]
                    .into(),
                )
            })
            .collect();

        (
            responses.get_external_token(),
            vec![(schema::GROUPS.to_string(), readable_pinfo.into())].into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        format_done(input.0, &input.1)
    }
}

/// The one `^done` completion record shape shared by aggregate presenters.
#[inline]
fn format_done(token: Option<u64>, payload: &Dict) -> String {
    MIFormatter::format("^", "done", Some(payload), token)
}

#[inline]
pub fn format_error(err_msg: &str, token: Option<u64>) -> String {
    let payload: Dict = HashMap::from([("msg".to_string(), err_msg.to_string().into())]).into();
    MIFormatter::format("^", "error", Some(&payload), token)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd_flow::ParsedSessionResponse;

    fn completion(sid: u64, payload: Dict) -> FinishedCmd {
        FinishedCmd::new(
            Some(11),
            sid,
            vec![ParsedSessionResponse::new(
                sid,
                "done".to_string(),
                Some(payload),
            )],
        )
    }

    #[test]
    fn thread_formatter_only_aggregates_preprojected_ids() {
        let payload: Dict = vec![
            (
                "threads".to_string(),
                Value::List(vec![Value::Dict(
                    vec![("id".to_string(), Value::from("41"))].into(),
                )]),
            ),
            ("current-thread-id".to_string(), Value::from("41")),
        ]
        .into();

        let transformed = ThreadInfoFormatter.transform(completion(3, payload));

        assert_eq!(
            transformed.1["threads"].expect_list_ref().unwrap()[0]
                .expect_dict_ref()
                .unwrap()["id"]
                .expect_string_ref()
                .unwrap(),
            "41"
        );
        assert_eq!(
            transformed.1["current-thread-id"]
                .expect_string_ref()
                .unwrap(),
            "41"
        );
    }

    #[test]
    fn readable_process_formatter_preserves_the_full_global_id() {
        let payload: Dict = vec![(
            "groups".to_string(),
            Value::List(vec![Value::Dict(
                vec![
                    ("id".to_string(), Value::from("42")),
                    ("type".to_string(), Value::from("process")),
                    ("pid".to_string(), Value::from("9001")),
                ]
                .into(),
            )]),
        )]
        .into();

        let transformed = ProcessReadableFormatter.transform(completion(7, payload));
        let process = transformed.1["groups"].expect_list_ref().unwrap()[0]
            .expect_dict_ref()
            .unwrap();
        assert_eq!(process["id"].expect_string_ref().unwrap(), "42");
        assert_eq!(process["desc"].expect_string_ref().unwrap(), "process 9001");
    }
}
