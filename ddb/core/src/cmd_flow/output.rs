use std::collections::HashMap;

use gdbmi::raw::{Dict, Value};
use tracing::debug;

use crate::{dbg_parser::gdb_parser::MIFormatter, state::STATES};

use super::FinishedCmd;

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

        for resp in responses.get_responses() {
            let sid = resp.get_sid();
            if let Some(payload) = resp.get_payload() {
                if let Value::List(threads) = &payload["threads"] {
                    for t in threads {
                        let mut t = t.expect_dict_ref().unwrap().clone();
                        let gtid = {
                            let tid = t["id"].expect_string_ref().unwrap();
                            let tid = tid.parse::<u64>().unwrap();
                            STATES.global_thread_id(sid, tid).unwrap()
                        };
                        t.insert("id".into(), Value::String(gtid.to_string()));
                        all_thread_info.push(t.into());
                    }
                }
            }
        }

        (
            responses.get_external_token(),
            vec![
                ("threads".to_string(), all_thread_info.into()),
                (
                    "current-thread-id".to_string(),
                    STATES
                        .current_thread_id()
                        .map(|v| v.to_string())
                        .unwrap_or("".to_string())
                        .into(),
                ),
            ]
            .into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// handle `-list-thread-groups` command response
#[derive(Clone)]
pub struct ProcessInfoFormatter;
impl Formatter for ProcessInfoFormatter {
    type Transformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        let mut all_process_info = Vec::<Value>::new();

        for resp in responses.get_responses() {
            let sid = resp.get_sid();
            if let Some(payload) = resp.get_payload() {
                if let Value::List(processes) = &payload["groups"] {
                    for p in processes {
                        let mut p = p.expect_dict_ref().unwrap().clone();
                        let gtgid = {
                            let tgid = p["id"].expect_string_ref().unwrap();
                            let gtgid = STATES.global_thread_group_id(sid, tgid).unwrap();
                            gtgid.to_string()
                        };
                        p.insert("id".into(), Value::String(gtgid));
                        all_process_info.push(p.into());
                    }
                }
            }
        }

        (
            responses.get_external_token(),
            vec![("groups".to_string(), all_process_info.into())].into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// handle `info inferiors` command response
#[derive(Clone)]
pub struct ProcessReadableFormatter;
impl Formatter for ProcessReadableFormatter {
    type Transformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        let (token, pinfo) = ProcessInfoFormatter.transform(responses);
        let grps = pinfo["groups"].expect_list_ref().unwrap();
        let readable_pinfo: Vec<Value> = grps
            .iter()
            .map(|p| {
                let p = p.expect_dict_ref().unwrap();
                let id = p["id"].expect_string_ref().unwrap()[1..].to_string();
                let ptype = p["type"].expect_string_ref().unwrap();
                let pid = p["pid"].expect_string_ref().unwrap();
                let exec = p
                    .get("executable")
                    .unwrap_or(&Value::String("".to_string()))
                    .clone();

                Value::Dict(
                    vec![
                        ("id".to_string(), id.into()),
                        ("desc".to_string(), format!("{} {}", ptype, pid).into()),
                        ("executable".to_string(), exec),
                    ]
                    .into(),
                )
            })
            .collect();

        (
            token,
            vec![("groups".to_string(), readable_pinfo.into())].into(),
        )
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

/// handle `-thread-select`
#[derive(Clone)]
pub struct ThreadSelectFormatter(u64); // gtid
impl ThreadSelectFormatter {
    #[allow(unused)]
    pub fn new(gtid: u64) -> Self {
        Self(gtid)
    }
}

impl Formatter for ThreadSelectFormatter {
    type Transformed = (Option<u64>, Dict);

    #[inline]
    fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
        let mut payload = responses
            .get_responses()
            .first()
            .unwrap()
            .get_payload()
            .unwrap()
            .clone();
        payload.insert("new-thread-id".into(), Value::String(self.0.to_string()));
        (responses.get_external_token(), payload)
    }

    #[inline]
    fn format(&self, input: &Self::Transformed) -> String {
        MIFormatter::format("^", "done", Some(&input.1), input.0)
    }
}

// /// handle `-break-insert`
// #[derive(Clone)]
// pub struct BreakInsertFormatter(u64); // gtid
// impl BreakInsertFormatter {
//     #[allow(unused)]
//     pub fn new(gtid: u64) -> Self {
//         Self(gtid)
//     }
// }

// impl Formatter for BreakInsertFormatter {
//     type Transformed = (Option<u64>, Dict);

//     #[inline]
//     fn transform(&self, responses: FinishedCmd) -> Self::Transformed {
//         let mut payload = responses
//             .get_responses()
//             .first()
//             .unwrap()
//             .get_payload()
//             .unwrap()
//             .clone();
//         payload.insert("new-thread-id".into(), Value::String(self.0.to_string()));
//         (responses.get_external_token(), payload)
//     }

//     #[inline]
//     fn format(&self, input: &Self::Transformed) -> String {
//         MIFormatter::format("^", "done", Some(&input.1), input.0)
//     }
// }

/// static dispatched version of the emit based on the formatter.
/// this is useful when the formatter is known at compile time.
#[inline]
pub fn emit_static<T: Formatter>(finished: FinishedCmd, formatter: T) {
    let transformed = formatter.transform(finished);
    let formatted = formatter.format(&transformed);
    println!("{}", formatted);
    debug!("output: {}", formatted);
}

#[inline]
pub fn format_error(err_msg: &str, token: Option<u64>) -> String {
    let payload: Dict = HashMap::from([("msg".to_string(), err_msg.to_string().into())]).into();
    MIFormatter::format("^", "error", Some(&payload), token)
}
