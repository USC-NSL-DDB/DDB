use std::collections::{HashMap, HashSet};

use ddb_api_types::v2::DdbErrorCode;

use crate::{
    cmd_flow::CommandOutcome,
    debugger::protocol::{Dict, Value},
};

use super::ApplicationError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedFrame {
    pub(crate) level: u32,
    pub(crate) function_name: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) line: u32,
    pub(crate) address: Option<String>,
    pub(crate) module: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedVariable {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) type_name: Option<String>,
    pub(crate) child_count: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedVariableChild {
    pub(crate) object_name: String,
    pub(crate) display_name: String,
    pub(crate) value: String,
    pub(crate) type_name: Option<String>,
    pub(crate) child_count: Option<u64>,
    pub(crate) presentation_hint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedVariableChildren {
    pub(crate) children: Vec<DecodedVariableChild>,
    pub(crate) has_more: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedMemory {
    pub(crate) address: String,
    pub(crate) data: Vec<u8>,
    pub(crate) unreadable_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedSignal {
    pub(crate) name: String,
    pub(crate) stop: bool,
    pub(crate) print: bool,
    pub(crate) pass: bool,
    pub(crate) description: Option<String>,
}

/// Checked boundary between backend dictionary payloads and the stable public
/// frame contract. No transport adapter is allowed to decode MI-shaped values
/// independently.
pub(crate) fn decode_frames(
    outcome: &CommandOutcome,
) -> Result<Vec<DecodedFrame>, ApplicationError> {
    let completion = outcome
        .response_ref()
        .ok_or_else(|| malformed("debugger returned no stack response"))?;
    let responses = completion.get_responses();
    if responses.len() != 1 {
        return Err(malformed(
            "thread-scoped stack query returned an unexpected response count",
        ));
    }
    let response = &responses[0];
    if response.get_message() != "done" {
        return Err(ApplicationError::new(
            DdbErrorCode::BackendFailed,
            "debugger rejected the stack query",
        ));
    }
    let payload = response
        .get_payload()
        .ok_or_else(|| malformed("debugger stack response has no payload"))?;
    let stack = list(payload, "stack")?;
    stack
        .iter()
        .map(|value| {
            let frame = dict(value, "stack[]")?;
            Ok(DecodedFrame {
                level: required_u32(frame, "level")?,
                function_name: optional_string(frame, "func")?,
                path: optional_string(frame, "fullname")?.or(optional_string(frame, "file")?),
                line: optional_u32(frame, "line")?.unwrap_or(0),
                address: optional_string(frame, "addr")?,
                module: optional_string(frame, "from")?,
            })
        })
        .collect()
}

pub(crate) fn decode_variables(
    outcome: &CommandOutcome,
) -> Result<Vec<DecodedVariable>, ApplicationError> {
    let completion = outcome
        .response_ref()
        .ok_or_else(|| malformed("debugger returned no variable response"))?;
    let responses = completion.get_responses();
    if responses.len() != 1 {
        return Err(malformed(
            "frame-scoped variable query returned an unexpected response count",
        ));
    }
    let response = &responses[0];
    if response.get_message() != "done" {
        return Err(ApplicationError::backend(
            "debugger rejected the variable query",
        ));
    }
    let payload = response
        .get_payload()
        .ok_or_else(|| malformed("debugger variable response has no payload"))?;
    let variables = match payload.get("variables") {
        Some(Value::List(values)) => values,
        Some(_) => return Err(malformed("debugger variable collection has the wrong type")),
        None => {
            return Err(malformed(
                "debugger variable response is missing its collection",
            ))
        }
    };
    variables
        .iter()
        .map(|value| {
            let variable = dict(value, "variables[]")?;
            let name = optional_string(variable, "name")?
                .filter(|name| !name.is_empty())
                .ok_or_else(|| malformed("debugger variable is missing its name"))?;
            let child_count = optional_string(variable, "numchild")?
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| malformed("debugger variable has an invalid child count"))
                })
                .transpose()?;
            Ok(DecodedVariable {
                name,
                value: optional_string(variable, "value")?.unwrap_or_default(),
                type_name: optional_string(variable, "type")?,
                child_count,
            })
        })
        .collect()
}

pub(crate) fn decode_signals(
    outcome: &CommandOutcome,
) -> Result<Vec<DecodedSignal>, ApplicationError> {
    let payload = single_done_payload(outcome, "signal")?;
    let signals = match payload.get("signals") {
        Some(Value::List(values)) => values,
        Some(_) => return Err(malformed("debugger signal collection has the wrong type")),
        None => {
            return Err(malformed(
                "debugger signal response is missing its collection",
            ))
        }
    };
    let mut names = HashSet::with_capacity(signals.len());
    signals
        .iter()
        .map(|value| {
            let signal = dict(value, "signals[]")?;
            let name = optional_string(signal, "name")?
                .filter(|name| !name.is_empty())
                .ok_or_else(|| malformed("debugger signal is missing its name"))?;
            if !names.insert(name.clone()) {
                return Err(malformed(
                    "debugger signal response contains a duplicate signal",
                ));
            }
            Ok(DecodedSignal {
                name,
                stop: signal_bool(signal, "stop")?,
                print: signal_bool(signal, "print")?,
                pass: signal_bool(signal, "pass")?,
                description: optional_string(signal, "description")?,
            })
        })
        .collect()
}

fn signal_bool(signal: &Dict, field: &'static str) -> Result<bool, ApplicationError> {
    match optional_string(signal, field)?
        .ok_or_else(|| malformed("debugger signal is missing a disposition field"))?
        .to_ascii_lowercase()
        .as_str()
    {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(malformed(format!(
            "debugger signal has an invalid {field} disposition",
        ))),
    }
}

pub(crate) fn decode_register_names(
    outcome: &CommandOutcome,
) -> Result<Vec<String>, ApplicationError> {
    let payload = single_done_payload(outcome, "register-name")?;
    let names = match payload.get("register-names") {
        Some(Value::List(values)) => values,
        Some(_) => {
            return Err(malformed(
                "debugger register-name collection has the wrong type",
            ))
        }
        None => {
            return Err(malformed(
                "debugger register-name response is missing its collection",
            ))
        }
    };
    names
        .iter()
        .map(|value| match value {
            Value::String(value) => Ok(value.clone()),
            _ => Err(malformed(
                "debugger register-name list contains a non-string value",
            )),
        })
        .collect()
}

pub(crate) fn decode_register_values(
    outcome: &CommandOutcome,
) -> Result<HashMap<u32, String>, ApplicationError> {
    let payload = single_done_payload(outcome, "register-value")?;
    let values = match payload.get("register-values") {
        Some(Value::List(values)) => values,
        Some(_) => {
            return Err(malformed(
                "debugger register-value collection has the wrong type",
            ))
        }
        None => {
            return Err(malformed(
                "debugger register-value response is missing its collection",
            ))
        }
    };
    let mut decoded = HashMap::with_capacity(values.len());
    for value in values {
        let register = dict(value, "register-values[]")?;
        let number = optional_string(register, "number")?
            .ok_or_else(|| malformed("debugger register value is missing its number"))?
            .parse::<u32>()
            .map_err(|_| malformed("debugger register value has an invalid number"))?;
        let value = optional_string(register, "value")?
            .ok_or_else(|| malformed("debugger register value is missing its value"))?;
        if decoded.insert(number, value).is_some() {
            return Err(malformed(
                "debugger register-value response contains a duplicate register",
            ));
        }
    }
    Ok(decoded)
}

pub(crate) fn decode_memory(
    outcome: &CommandOutcome,
    requested_address: &str,
    requested_byte_count: u64,
) -> Result<DecodedMemory, ApplicationError> {
    let payload = single_done_payload(outcome, "memory")?;
    let records = match payload.get("memory") {
        Some(Value::List(values)) => values,
        Some(_) => return Err(malformed("debugger memory collection has the wrong type")),
        None => {
            return Err(malformed(
                "debugger memory response is missing its collection",
            ))
        }
    };

    let mut chunks = Vec::with_capacity(records.len());
    for record in records {
        let record = dict(record, "memory[]")?;
        let offset = optional_string(record, "offset")?
            .map(|value| parse_mi_u64(&value))
            .transpose()?
            .unwrap_or(0);
        let begin = optional_string(record, "begin")?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| malformed("debugger memory record is missing its address"))?;
        let contents = optional_string(record, "contents")?
            .ok_or_else(|| malformed("debugger memory record is missing its contents"))?;
        let bytes = decode_hex(&contents)?;
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| malformed("debugger memory range overflowed"))?;
        if end > requested_byte_count {
            return Err(malformed(
                "debugger returned more memory than the requested bound",
            ));
        }
        chunks.push((offset, begin, bytes));
    }
    chunks.sort_unstable_by_key(|(offset, _, _)| *offset);

    let mut address = requested_address.to_string();
    let mut data = Vec::with_capacity(requested_byte_count as usize);
    let mut cursor = 0_u64;
    for (offset, begin, bytes) in chunks {
        if offset < cursor {
            return Err(malformed(
                "debugger memory response contains overlapping ranges",
            ));
        }
        if offset > cursor {
            break;
        }
        if cursor == 0 {
            address = begin;
        }
        cursor = cursor
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| malformed("debugger memory range overflowed"))?;
        data.extend(bytes);
    }

    Ok(DecodedMemory {
        address,
        data,
        unreadable_bytes: requested_byte_count.saturating_sub(cursor),
    })
}

pub(crate) fn decode_variable_object_name(
    outcome: &CommandOutcome,
) -> Result<String, ApplicationError> {
    let payload = single_done_payload(outcome, "variable-object creation")?;
    optional_string(payload, "name")?
        .filter(|name| !name.is_empty())
        .ok_or_else(|| malformed("debugger variable object is missing its name"))
}

pub(crate) fn decode_variable_children(
    outcome: &CommandOutcome,
) -> Result<DecodedVariableChildren, ApplicationError> {
    let payload = single_done_payload(outcome, "variable-child")?;
    let children = match payload.get("children") {
        Some(Value::List(values)) => values,
        Some(_) => return Err(malformed("debugger variable children have the wrong type")),
        None => {
            return Err(malformed(
                "debugger variable-child response is missing its collection",
            ))
        }
    };
    let children = children
        .iter()
        .map(|value| {
            let child = dict(value, "children[]")?;
            let object_name = optional_string(child, "name")?
                .filter(|name| !name.is_empty())
                .ok_or_else(|| malformed("debugger variable child is missing its object name"))?;
            let display_name = optional_string(child, "exp")?
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| object_name.clone());
            let child_count = optional_string(child, "numchild")?
                .map(|value| {
                    value.parse::<u64>().map_err(|_| {
                        malformed("debugger variable child has an invalid child count")
                    })
                })
                .transpose()?;
            Ok(DecodedVariableChild {
                object_name,
                display_name,
                value: optional_string(child, "value")?.unwrap_or_default(),
                type_name: optional_string(child, "type")?,
                child_count,
                presentation_hint: optional_string(child, "displayhint")?,
            })
        })
        .collect::<Result<Vec<_>, ApplicationError>>()?;
    let has_more = match optional_string(payload, "has_more")?.as_deref() {
        None | Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(_) => {
            return Err(malformed(
                "debugger variable-child response has an invalid has_more value",
            ))
        }
    };
    if has_more && children.is_empty() {
        return Err(malformed(
            "debugger reported more variable children without returning progress",
        ));
    }
    Ok(DecodedVariableChildren { children, has_more })
}

pub(crate) fn decode_empty_done(
    outcome: &CommandOutcome,
    subject: &'static str,
) -> Result<(), ApplicationError> {
    let completion = outcome
        .response_ref()
        .ok_or_else(|| malformed(format!("debugger returned no {subject} response")))?;
    let responses = completion.get_responses();
    if responses.len() != 1 {
        return Err(malformed(format!(
            "target-scoped {subject} command returned an unexpected response count"
        )));
    }
    if responses[0].get_message() != "done" {
        return Err(ApplicationError::backend(format!(
            "debugger rejected the {subject} command"
        )));
    }
    Ok(())
}

fn single_done_payload<'a>(
    outcome: &'a CommandOutcome,
    subject: &'static str,
) -> Result<&'a Dict, ApplicationError> {
    let completion = outcome
        .response_ref()
        .ok_or_else(|| malformed(format!("debugger returned no {subject} response")))?;
    let responses = completion.get_responses();
    if responses.len() != 1 {
        return Err(malformed(format!(
            "target-scoped {subject} query returned an unexpected response count"
        )));
    }
    let response = &responses[0];
    if response.get_message() != "done" {
        return Err(ApplicationError::backend(format!(
            "debugger rejected the {subject} query"
        )));
    }
    response
        .get_payload()
        .ok_or_else(|| malformed(format!("debugger {subject} response has no payload")))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ApplicationError> {
    if !value.len().is_multiple_of(2) {
        return Err(malformed(
            "debugger memory contents contain an incomplete byte",
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().as_chunks::<2>().0 {
        let high = hex_nibble(pair[0])
            .ok_or_else(|| malformed("debugger memory contents are not hexadecimal"))?;
        let low = hex_nibble(pair[1])
            .ok_or_else(|| malformed("debugger memory contents are not hexadecimal"))?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn parse_mi_u64(value: &str) -> Result<u64, ApplicationError> {
    let parsed = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse::<u64>(),
            |digits| u64::from_str_radix(digits, 16),
        );
    parsed.map_err(|_| malformed("debugger memory record has an invalid offset"))
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn list<'a>(value: &'a Dict, field: &'static str) -> Result<&'a [Value], ApplicationError> {
    match value.get(field) {
        Some(Value::List(values)) => Ok(values),
        Some(_) => Err(malformed("debugger stack collection has the wrong type")),
        None => Err(malformed(
            "debugger stack response is missing its collection",
        )),
    }
}

fn dict<'a>(value: &'a Value, field: &'static str) -> Result<&'a Dict, ApplicationError> {
    match value {
        Value::Dict(value) => Ok(value),
        _ => Err(malformed(match field {
            "stack[]" => "debugger stack contains a non-frame record",
            "variables[]" => "debugger variable list contains a malformed record",
            _ => "debugger response contains a malformed record",
        })),
    }
}

fn required_u32(value: &Dict, field: &'static str) -> Result<u32, ApplicationError> {
    optional_u32(value, field)?.ok_or_else(|| malformed("debugger frame is missing its level"))
}

fn optional_u32(value: &Dict, field: &'static str) -> Result<Option<u32>, ApplicationError> {
    optional_string(value, field)?
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| malformed("debugger frame contains an invalid integer"))
        })
        .transpose()
}

fn optional_string(value: &Dict, field: &'static str) -> Result<Option<String>, ApplicationError> {
    match value.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(malformed("debugger response contains a non-string field")),
        None => Ok(None),
    }
}

fn malformed(message: impl Into<String>) -> ApplicationError {
    ApplicationError::new(DdbErrorCode::BackendFailed, message)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::cmd_flow::{FinishedCmd, ParsedSessionResponse, Presentation};

    use super::*;

    fn outcome(payload: Dict) -> CommandOutcome {
        CommandOutcome::response(
            FinishedCmd::new(
                None,
                1,
                vec![ParsedSessionResponse::new(
                    1,
                    "done".to_string(),
                    Some(payload),
                )],
            ),
            Presentation::Plain,
        )
    }

    #[test]
    fn decodes_backend_frames_without_leaking_dictionary_shape() {
        let frame = Dict::new(HashMap::from([
            ("level".to_string(), Value::String("0".to_string())),
            ("func".to_string(), Value::String("main".to_string())),
            (
                "fullname".to_string(),
                Value::String("/src/main.rs".to_string()),
            ),
            ("line".to_string(), Value::String("19".to_string())),
            ("addr".to_string(), Value::String("0x10".to_string())),
        ]));
        let payload = Dict::new(HashMap::from([(
            "stack".to_string(),
            Value::List(vec![Value::Dict(frame)]),
        )]));

        assert_eq!(
            decode_frames(&outcome(payload)).unwrap(),
            vec![DecodedFrame {
                level: 0,
                function_name: Some("main".to_string()),
                path: Some("/src/main.rs".to_string()),
                line: 19,
                address: Some("0x10".to_string()),
                module: None,
            }]
        );
    }

    #[test]
    fn rejects_missing_or_mistyped_frame_levels() {
        for frame in [
            Dict::new(HashMap::new()),
            Dict::new(HashMap::from([(
                "level".to_string(),
                Value::List(Vec::new()),
            )])),
        ] {
            let payload = Dict::new(HashMap::from([(
                "stack".to_string(),
                Value::List(vec![Value::Dict(frame)]),
            )]));
            assert_eq!(
                decode_frames(&outcome(payload)).unwrap_err().code(),
                DdbErrorCode::BackendFailed
            );
        }
    }

    #[test]
    fn decodes_typed_variables_and_rejects_invalid_child_counts() {
        let variable = Dict::new(HashMap::from([
            ("name".to_string(), Value::String("request".to_string())),
            ("value".to_string(), Value::String("0x1000".to_string())),
            ("type".to_string(), Value::String("Request *".to_string())),
            ("numchild".to_string(), Value::String("3".to_string())),
        ]));
        let payload = Dict::new(HashMap::from([(
            "variables".to_string(),
            Value::List(vec![Value::Dict(variable)]),
        )]));
        assert_eq!(
            decode_variables(&outcome(payload)).unwrap(),
            vec![DecodedVariable {
                name: "request".to_string(),
                value: "0x1000".to_string(),
                type_name: Some("Request *".to_string()),
                child_count: Some(3),
            }]
        );

        let invalid = Dict::new(HashMap::from([
            ("name".to_string(), Value::String("request".to_string())),
            ("numchild".to_string(), Value::String("many".to_string())),
        ]));
        let payload = Dict::new(HashMap::from([(
            "variables".to_string(),
            Value::List(vec![Value::Dict(invalid)]),
        )]));
        assert_eq!(
            decode_variables(&outcome(payload)).unwrap_err().code(),
            DdbErrorCode::BackendFailed
        );
    }

    #[test]
    fn decodes_register_names_and_rejects_duplicate_values() {
        let payload = Dict::new(HashMap::from([(
            "register-names".to_string(),
            Value::List(vec![
                Value::String("rax".to_string()),
                Value::String(String::new()),
                Value::String("rip".to_string()),
            ]),
        )]));
        assert_eq!(
            decode_register_names(&outcome(payload)).unwrap(),
            vec!["rax", "", "rip"]
        );

        let register = || {
            Value::Dict(Dict::new(HashMap::from([
                ("number".to_string(), Value::String("0".to_string())),
                ("value".to_string(), Value::String("42".to_string())),
            ])))
        };
        let payload = Dict::new(HashMap::from([(
            "register-values".to_string(),
            Value::List(vec![register(), register()]),
        )]));
        assert_eq!(
            decode_register_values(&outcome(payload))
                .unwrap_err()
                .code(),
            DdbErrorCode::BackendFailed
        );
    }

    #[test]
    fn decodes_typed_signals_and_rejects_invalid_dispositions() {
        let signal = |stop: &str| {
            Value::Dict(Dict::new(HashMap::from([
                ("name".to_string(), Value::String("SIGINT".to_string())),
                ("stop".to_string(), Value::String(stop.to_string())),
                ("print".to_string(), Value::String("Yes".to_string())),
                ("pass".to_string(), Value::String("No".to_string())),
                (
                    "description".to_string(),
                    Value::String("Interrupt".to_string()),
                ),
            ])))
        };
        let payload = Dict::new(HashMap::from([(
            "signals".to_string(),
            Value::List(vec![signal("true")]),
        )]));
        assert_eq!(
            decode_signals(&outcome(payload)).unwrap(),
            vec![DecodedSignal {
                name: "SIGINT".to_string(),
                stop: true,
                print: true,
                pass: false,
                description: Some("Interrupt".to_string()),
            }]
        );

        let payload = Dict::new(HashMap::from([(
            "signals".to_string(),
            Value::List(vec![signal("sometimes")]),
        )]));
        assert_eq!(
            decode_signals(&outcome(payload)).unwrap_err().code(),
            DdbErrorCode::BackendFailed
        );
    }

    #[test]
    fn decodes_contiguous_memory_and_rejects_invalid_hex() {
        let chunk = |offset: &str, begin: &str, contents: &str| {
            Value::Dict(Dict::new(HashMap::from([
                ("offset".to_string(), Value::String(offset.to_string())),
                ("begin".to_string(), Value::String(begin.to_string())),
                ("contents".to_string(), Value::String(contents.to_string())),
            ])))
        };
        let payload = Dict::new(HashMap::from([(
            "memory".to_string(),
            Value::List(vec![
                chunk("0x2", "0x1002", "ff"),
                chunk("0X0", "0x1000", "2a00"),
            ]),
        )]));
        assert_eq!(
            decode_memory(&outcome(payload), "request-expression", 4).unwrap(),
            DecodedMemory {
                address: "0x1000".to_string(),
                data: vec![0x2a, 0, 0xff],
                unreadable_bytes: 1,
            }
        );

        let payload = Dict::new(HashMap::from([(
            "memory".to_string(),
            Value::List(vec![chunk("0", "0x1000", "xy")]),
        )]));
        assert_eq!(
            decode_memory(&outcome(payload), "0x1000", 1)
                .unwrap_err()
                .code(),
            DdbErrorCode::BackendFailed
        );

        let payload = Dict::new(HashMap::from([(
            "memory".to_string(),
            Value::List(vec![chunk("-1", "0x1000", "00")]),
        )]));
        assert_eq!(
            decode_memory(&outcome(payload), "0x1000", 1)
                .unwrap_err()
                .code(),
            DdbErrorCode::BackendFailed
        );
    }
}
