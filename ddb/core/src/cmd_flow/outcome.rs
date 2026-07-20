use gdbmi::raw::Dict;

use super::{
    output::{
        Formatter, PlainFormatter, ProcessReadableFormatter, ThreadInfoFormatter, UnitFormatter,
    },
    FinishedCmd, ParsedSessionResponse,
};

/// Presentation is an ingress concern. Command operations choose the semantic
/// shape of their result, while the CLI adapter decides whether to render it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Presentation {
    Plain,
    Unit,
    ThreadInfo,
    ProcessReadable,
}

#[derive(Debug, Clone)]
enum CliOutputPart {
    Response(Presentation),
    Record(String),
}

/// Structured result of one user-level command operation.
///
/// `response` is shared by HTTP, CLI, and internal callers. `cli_output` only
/// describes how the CLI adapter should present that same operation; command
/// command operations never print directly.
#[derive(Debug, Clone, Default)]
pub struct CommandOutcome {
    response: Option<FinishedCmd>,
    cli_output: Vec<CliOutputPart>,
}

impl CommandOutcome {
    pub fn response(response: FinishedCmd, presentation: Presentation) -> Self {
        Self {
            response: Some(response),
            cli_output: vec![CliOutputPart::Response(presentation)],
        }
    }

    pub fn silent(response: FinishedCmd) -> Self {
        Self {
            response: Some(response),
            cli_output: Vec::new(),
        }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn completed(
        external_token: Option<u64>,
        sid: u64,
        message: impl Into<String>,
        payload: Option<Dict>,
        presentation: Presentation,
    ) -> Self {
        Self::response(
            FinishedCmd::new(
                external_token,
                sid,
                vec![ParsedSessionResponse::new(sid, message.into(), payload)],
            ),
            presentation,
        )
    }

    pub fn push_record(&mut self, record: impl Into<String>) {
        self.cli_output.push(CliOutputPart::Record(record.into()));
    }

    pub fn insert_record(&mut self, index: usize, record: impl Into<String>) {
        self.cli_output
            .insert(index, CliOutputPart::Record(record.into()));
    }

    pub fn response_ref(&self) -> Option<&FinishedCmd> {
        self.response.as_ref()
    }

    pub fn into_response(self) -> Option<FinishedCmd> {
        self.response
    }

    pub fn render_cli(&self) -> Vec<String> {
        self.cli_output
            .iter()
            .filter_map(|part| match part {
                CliOutputPart::Record(record) => Some(record.clone()),
                CliOutputPart::Response(presentation) => self
                    .response
                    .as_ref()
                    .map(|response| render(response.clone(), *presentation)),
            })
            .collect()
    }
}

fn render(response: FinishedCmd, presentation: Presentation) -> String {
    match presentation {
        Presentation::Plain => render_with(response, PlainFormatter),
        Presentation::Unit => render_with(response, UnitFormatter),
        Presentation::ThreadInfo => render_with(response, ThreadInfoFormatter),
        Presentation::ProcessReadable => render_with(response, ProcessReadableFormatter),
    }
}

fn render_with<F: Formatter>(response: FinishedCmd, formatter: F) -> String {
    let transformed = formatter.transform(response);
    formatter.format(&transformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_presentation_is_lazy_and_structured_response_is_preserved() {
        let outcome = CommandOutcome::completed(Some(7), 3, "done", None, Presentation::Plain);
        assert_eq!(outcome.render_cli(), vec!["7^done"]);
        assert_eq!(outcome.response_ref().unwrap().get_sid(), 3);
    }

    #[test]
    fn silent_outcome_retains_response_without_cli_output() {
        let response = FinishedCmd::new(
            None,
            4,
            vec![ParsedSessionResponse::new(4, "done".to_string(), None)],
        );
        let outcome = CommandOutcome::silent(response);
        assert!(outcome.render_cli().is_empty());
        assert!(outcome.response_ref().is_some());
    }
}
