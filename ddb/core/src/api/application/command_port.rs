use async_trait::async_trait;
use ddb_api_types::v2::OperationKind;

use crate::cmd_flow::{
    engine::CommandEngine,
    router::{CommandFanoutReport, SessionCommandFailure, SessionCommandFailureKind, Target},
    CommandOutcome,
};

/// Command execution seam used by the public application service. The
/// production implementation is the same engine used by stdin and v1.
#[async_trait]
pub(crate) trait ApplicationCommandPort: Send + Sync {
    async fn execute(
        &self,
        command: &str,
        target: Target,
    ) -> Result<CommandOutcome, CommandPortError>;

    async fn execute_tracked(
        &self,
        command: &str,
        target: Target,
        operation_id: &str,
        operation_kind: OperationKind,
    ) -> Result<CommandOutcome, CommandPortError> {
        let _ = (operation_id, operation_kind);
        self.execute(command, target).await
    }
}

#[derive(Debug, thiserror::Error)]
#[error("debugger command failed")]
pub(crate) struct CommandPortError {
    #[source]
    source: anyhow::Error,
    fanout: Option<Box<CommandFanoutReport>>,
}

impl CommandPortError {
    #[cfg(test)]
    pub(crate) fn test(message: impl Into<String>) -> Self {
        Self {
            source: anyhow::anyhow!(message.into()),
            fanout: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_with_fanout(report: CommandFanoutReport) -> Self {
        Self {
            source: anyhow::anyhow!("synthetic fanout failure"),
            fanout: Some(Box::new(report)),
        }
    }

    pub(crate) fn fanout_report(&self) -> Option<&CommandFanoutReport> {
        self.fanout.as_deref()
    }

    fn from_command_error(error: crate::cmd_flow::engine::CommandError) -> Self {
        let fanout = error.fanout_report().cloned().map(Box::new);
        Self {
            source: anyhow::anyhow!(error),
            fanout,
        }
    }

    pub(crate) fn from_fanout(report: CommandFanoutReport) -> Self {
        Self {
            source: anyhow::anyhow!("debugger command failed for one or more session targets"),
            fanout: Some(Box::new(report)),
        }
    }

    fn classify_debugger_responses(
        outcome: CommandOutcome,
    ) -> Result<CommandOutcome, CommandPortError> {
        let Some(completion) = outcome.response_ref() else {
            return Ok(outcome);
        };
        let mut successes = Vec::new();
        let mut failures = Vec::new();
        for response in completion.get_responses() {
            if response.get_message() == "error" {
                failures.push(SessionCommandFailure::new(
                    response.get_sid(),
                    SessionCommandFailureKind::DebuggerRejected,
                ));
            } else {
                successes.push(response.clone());
            }
        }
        if failures.is_empty() {
            Ok(outcome)
        } else {
            Err(Self::from_fanout(CommandFanoutReport::new(
                completion.get_external_token(),
                successes,
                failures,
            )))
        }
    }
}

#[async_trait]
impl ApplicationCommandPort for CommandEngine {
    async fn execute(
        &self,
        command: &str,
        target: Target,
    ) -> Result<CommandOutcome, CommandPortError> {
        let outcome = self
            .execute_api(command, Some(target))
            .await
            .map_err(CommandPortError::from_command_error)?;
        CommandPortError::classify_debugger_responses(outcome)
    }

    async fn execute_tracked(
        &self,
        command: &str,
        target: Target,
        operation_id: &str,
        operation_kind: OperationKind,
    ) -> Result<CommandOutcome, CommandPortError> {
        let outcome = self
            .execute_api_with_metadata(
                command,
                Some(target),
                crate::cmd_flow::input::CommandMetadata {
                    operation_id: Some(operation_id.to_string()),
                    operation_kind: Some(operation_kind as u32),
                },
            )
            .await
            .map_err(CommandPortError::from_command_error)?;
        CommandPortError::classify_debugger_responses(outcome)
    }
}

#[cfg(test)]
pub(crate) struct NoopCommandPort;

#[cfg(test)]
#[async_trait]
impl ApplicationCommandPort for NoopCommandPort {
    async fn execute(
        &self,
        _command: &str,
        _target: Target,
    ) -> Result<CommandOutcome, CommandPortError> {
        Ok(CommandOutcome::empty())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        cmd_flow::{FinishedCmd, ParsedSessionResponse},
        debugger::protocol::{Dict, Value},
    };

    use super::*;

    #[test]
    fn debugger_error_records_become_safe_per_target_failures_only_at_the_v2_port() {
        let sensitive_payload: Dict = vec![(
            "msg".to_string(),
            Value::from("sensitive debugger diagnostic"),
        )]
        .into();
        let outcome = CommandOutcome::silent(FinishedCmd::new(
            Some(41),
            7,
            vec![
                ParsedSessionResponse::new(7, "done".to_string(), None),
                ParsedSessionResponse::new(8, "error".to_string(), Some(sensitive_payload)),
            ],
        ));

        let error = CommandPortError::classify_debugger_responses(outcome).unwrap_err();
        let report = error
            .fanout_report()
            .expect("structured fanout report should be retained");
        assert_eq!(report.completion().get_external_token(), Some(41));
        assert_eq!(report.completion().get_responses().len(), 1);
        assert_eq!(report.completion().get_responses()[0].get_sid(), 7);
        assert_eq!(
            report.failures(),
            &[SessionCommandFailure::new(
                8,
                SessionCommandFailureKind::DebuggerRejected
            )]
        );
        assert!(!error.to_string().contains("sensitive debugger"));
    }

    #[test]
    fn successful_debugger_records_pass_through_unchanged() {
        let outcome = CommandOutcome::silent(FinishedCmd::new(
            Some(42),
            9,
            vec![ParsedSessionResponse::new(9, "done".to_string(), None)],
        ));

        let classified = CommandPortError::classify_debugger_responses(outcome).unwrap();
        let response = classified.response_ref().unwrap();
        assert_eq!(response.get_external_token(), Some(42));
        assert_eq!(response.get_responses()[0].get_sid(), 9);
    }
}
