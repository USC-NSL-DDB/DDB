//! Logpoint templates keep literal output separate from debugger expressions.
//! Evaluation and continuation are tied to one canonical stop and control epoch.

const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
const MAX_EXPRESSIONS: usize = 128;

use anyhow::{ensure, Context};
use ddb_api_client::{v2, DdbClient};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[cfg(test)]
#[path = "logpoint_worker_tests.rs"]
mod worker_tests;

/// A logpoint may continue only the exact stop that initiated its evaluation.
pub struct Stop {
    pub state: v2::ExecutionState,
    pub server_instance: String,
    pub control_epoch: u64,
    pub current_epoch: Arc<AtomicU64>,
}

impl Stop {
    fn current(&self) -> bool {
        self.current_epoch.load(Ordering::SeqCst) == self.control_epoch
    }

    async fn still_stopped(&self, client: &DdbClient) -> anyhow::Result<bool> {
        if !self.current() {
            return Ok(false);
        }
        let response = client
            .get_execution_state(v2::GetExecutionStateRequest {
                target: self.state.target.clone(),
                ..Default::default()
            })
            .await?;
        self.check_instance(response.context)?;
        let latest = response
            .execution_state
            .context("DDB omitted current logpoint execution state")?;
        Ok(self.current()
            && !latest.running
            && latest.execution_state_id == self.state.execution_state_id
            && latest.revision == self.state.revision
            && latest.target == self.state.target)
    }

    fn check_instance(&self, context: Option<v2::ResponseContext>) -> anyhow::Result<()> {
        ensure!(
            context.is_some_and(|context| context.server_instance_id == self.server_instance),
            "DDB instance changed during logpoint evaluation"
        );
        Ok(())
    }
}

/// Returns false when explicit control or a newer stop supersedes this work.
/// Errors never trigger continuation. Output is emitted only after validation.
pub async fn run<F, Delivered>(
    client: &DdbClient,
    template: &Template,
    stop: &Stop,
    output: F,
) -> anyhow::Result<bool>
where
    F: FnOnce(String) -> Delivered,
    Delivered: std::future::Future<Output = anyhow::Result<()>>,
{
    if !stop.current() {
        return Ok(false);
    }
    let Some(v2::target::Selector::Thread(owner)) = stop
        .state
        .target
        .as_ref()
        .and_then(|target| target.selector.as_ref())
    else {
        anyhow::bail!("Logpoint stop must identify one thread");
    };
    ensure!(
        !stop.state.running && !stop.state.execution_state_id.is_empty(),
        "Logpoint requires a stopped execution identity"
    );
    ensure!(
        stop.state
            .stop_reason
            .as_ref()
            .is_some_and(
                |reason| reason.kind == v2::StopReasonKind::Breakpoint as i32
                    && reason
                        .thread_id
                        .as_ref()
                        .is_none_or(|thread| thread == &owner.thread_id)
            ),
        "Logpoint stop belongs to a different cause or thread"
    );
    let target = stop.state.target.clone();
    let frame_id = if template
        .parts
        .iter()
        .any(|part| matches!(part, Part::Expression(_)))
    {
        let response = client
            .list_frames(v2::ListFramesRequest {
                thread_id: owner.thread_id.clone(),
                page: Some(v2::PageRequest {
                    page_size: 1,
                    page_token: None,
                }),
                ..Default::default()
            })
            .await?;
        stop.check_instance(response.context)?;
        Some(
            response
                .frames
                .first()
                .filter(|frame| !frame.frame_id.is_empty())
                .context("DDB returned no frame for the logpoint expression")?
                .frame_id
                .clone(),
        )
    } else {
        None
    };
    let mut message = String::new();
    for part in &template.parts {
        if !stop.current() {
            return Ok(false);
        }
        let value = match part {
            Part::Text(text) => text.clone(),
            Part::Expression(expression) => {
                let admission = client
                    .evaluate(v2::EvaluateRequest {
                        target: target.clone(),
                        frame_id: frame_id.clone(),
                        expression: expression.clone(),
                        evaluation_context: v2::EvaluationContext::Watch as i32,
                        ..Default::default()
                    })
                    .await?;
                stop.check_instance(admission.context.clone())?;
                let receipt = crate::worker::complete_operation(client, admission).await?;
                ensure!(
                    receipt.state == v2::OperationState::Completed as i32,
                    "Logpoint evaluation failed"
                );
                match receipt.result.and_then(|result| result.value) {
                    Some(v2::operation_result::Value::Evaluation(value)) => value.value,
                    _ => anyhow::bail!("DDB omitted the logpoint expression value"),
                }
            }
        };
        ensure!(
            message.len().saturating_add(value.len()) <= MAX_TEMPLATE_BYTES,
            "Logpoint output exceeds 64 KiB"
        );
        message.push_str(&value);
    }
    if !stop.still_stopped(client).await? {
        return Ok(false);
    }
    if !message.ends_with('\n') {
        ensure!(
            message.len() < MAX_TEMPLATE_BYTES,
            "Logpoint output exceeds 64 KiB"
        );
        message.push('\n');
    }
    output(message).await?;
    // Output delivery can wait on a full queue. Re-check the backend as well
    // as user control after that await, before admitting a continuation.
    if !stop.still_stopped(client).await? {
        return Ok(false);
    }
    let admission = client
        .execute(v2::ExecuteRequest {
            target,
            action: v2::ExecutionAction::Continue as i32,
            ..Default::default()
        })
        .await?;
    stop.check_instance(admission.context.clone())?;
    let receipt = crate::worker::complete_operation(client, admission).await?;
    ensure!(
        receipt.state == v2::OperationState::Completed as i32,
        "Logpoint continuation failed"
    );
    Ok(true)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Part {
    Text(String),
    Expression(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    parts: Vec<Part>,
}

impl Template {
    pub fn parse(message: &str) -> Result<Self, String> {
        if message.len() > MAX_TEMPLATE_BYTES {
            return Err("Log message exceeds 64 KiB".into());
        }
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut expressions = 0;
        let mut chars = message.char_indices().peekable();
        while let Some((offset, ch)) = chars.next() {
            if matches!(ch, '{' | '}') && chars.peek().is_some_and(|(_, next)| *next == ch) {
                chars.next();
                text.push(ch);
                continue;
            }
            match ch {
                '}' => return Err("Unmatched closing brace in log message".into()),
                '{' => {}
                _ => {
                    text.push(ch);
                    continue;
                }
            }
            if !text.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut text)));
            }
            let start = offset + 1;
            let mut depth = 1;
            let mut quote = None;
            let mut escaped = false;
            let mut end = None;
            for (offset, ch) in chars.by_ref() {
                if let Some(delimiter) = quote {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == delimiter {
                        quote = None;
                    }
                } else {
                    match ch {
                        '\'' | '"' => quote = Some(ch),
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(offset);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            let end = end.ok_or("Unmatched opening brace in log message")?;
            let expression = message[start..end].trim();
            if expression.is_empty() {
                return Err("Logpoint expression cannot be empty".into());
            }
            expressions += 1;
            if expressions > MAX_EXPRESSIONS {
                return Err("Log message exceeds 128 expressions".into());
            }
            parts.push(Part::Expression(expression.into()));
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        Ok(Self { parts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_never_debugger_expressions() {
        assert_eq!(Template::parse("").unwrap().parts, []);
        assert_eq!(
            Template::parse("literal; $pc = 0 {{braces}}")
                .unwrap()
                .parts,
            [Part::Text("literal; $pc = 0 {braces}".into())]
        );
        assert_eq!(
            Template::parse("你好 {{count}} = { counter }!")
                .unwrap()
                .parts,
            [
                Part::Text("你好 {count} = ".into()),
                Part::Expression("counter".into()),
                Part::Text("!".into())
            ]
        );
    }

    #[test]
    fn nested_braces_quotes_and_escapes_stay_inside_expressions() {
        let expression = r#"call({1, 2}, "}\\\"{", '}')"#;
        assert_eq!(
            Template::parse(&format!("result={{{expression}}}"))
                .unwrap()
                .parts,
            [
                Part::Text("result=".into()),
                Part::Expression(expression.into())
            ]
        );
        assert_eq!(
            Template::parse("{{{value}}}").unwrap().parts,
            [
                Part::Text("{".into()),
                Part::Expression("value".into()),
                Part::Text("}".into())
            ]
        );
    }

    #[test]
    fn invalid_templates_fail_before_any_expression_can_run() {
        for input in [
            "}",
            "{",
            "{}",
            "{  }",
            "{value",
            "{\"unterminated}",
            "good {x} bad }",
        ] {
            assert!(Template::parse(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn limits_count_utf8_bytes_and_expressions_not_characters() {
        assert!(Template::parse(&"x".repeat(MAX_TEMPLATE_BYTES)).is_ok());
        assert!(Template::parse(&"x".repeat(MAX_TEMPLATE_BYTES + 1)).is_err());
        assert!(Template::parse(&"é".repeat(MAX_TEMPLATE_BYTES / 2)).is_ok());
        assert!(Template::parse(&"é".repeat(MAX_TEMPLATE_BYTES / 2 + 1)).is_err());
        assert!(Template::parse(&"{x}".repeat(MAX_EXPRESSIONS)).is_ok());
        assert!(Template::parse(&"{x}".repeat(MAX_EXPRESSIONS + 1)).is_err());
    }
}
