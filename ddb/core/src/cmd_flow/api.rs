//! Low-level semantic command execution used by command operations.
//!
//! User ingress belongs to `CommandEngine`. This API is intentionally
//! completion-oriented for operations that need to compose several debugger
//! commands without exposing transport channels or presentation concerns.

use anyhow::{Context, Result};
use std::{fmt, sync::Arc};

use super::{
    input::{Command, ParsedInputCmd},
    router::Router,
    FinishedCmd,
};

pub use super::router::Target;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid command prefix: {0}")]
    InvalidPrefix(String),
    #[error("Command execution failed: {0}")]
    Execution(#[from] anyhow::Error),
}

/// Executes internal debugger commands through an explicitly owned router.
///
/// Application services use this instead of rediscovering the process-wide
/// router for every command they compose.
#[derive(Clone)]
pub(crate) struct CommandExecutor {
    router: Arc<Router>,
}

impl CommandExecutor {
    pub(crate) fn new(router: Arc<Router>) -> Self {
        Self { router }
    }

    pub(crate) async fn execute(&self, command_text: &str, target: Target) -> Result<FinishedCmd> {
        self.execute_plan(command(command_text)?.target(target))
            .await
    }

    pub(crate) async fn execute_parsed(&self, command: ParsedInputCmd) -> Result<FinishedCmd> {
        self.execute_plan(CommandPlan::from_parsed(command)?).await
    }

    pub(crate) async fn execute_exclusive(
        &self,
        command_text: &str,
        target: Target,
        lease: &super::session_runtime::SessionLease,
    ) -> Result<FinishedCmd> {
        self.execute_plan_exclusive(command(command_text)?.target(target), lease)
            .await
    }

    pub(crate) async fn execute_parsed_exclusive(
        &self,
        command: ParsedInputCmd,
        target: Target,
        lease: &super::session_runtime::SessionLease,
    ) -> Result<FinishedCmd> {
        self.execute_plan_exclusive(CommandPlan::from_parsed(command)?.target(target), lease)
            .await
    }

    pub(crate) async fn execute_plan(&self, plan: CommandPlan) -> Result<FinishedCmd> {
        let (target, command) = plan.into_parts(self.router.next_internal_token());
        self.router.execute(target, command).await
    }

    pub(crate) async fn execute_plan_exclusive(
        &self,
        plan: CommandPlan,
        lease: &super::session_runtime::SessionLease,
    ) -> Result<FinishedCmd> {
        let (target, command) = plan.into_parts(self.router.next_internal_token());
        self.router.execute_exclusive(lease, target, command).await
    }

    pub(crate) async fn execute_plan_with_optional_lease(
        &self,
        plan: CommandPlan,
        lease: Option<&super::session_runtime::SessionLease>,
    ) -> Result<FinishedCmd> {
        match lease {
            Some(lease) => self.execute_plan_exclusive(plan, lease).await,
            None => self.execute_plan(plan).await,
        }
    }
}

impl fmt::Debug for CommandExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("CommandExecutor").finish()
    }
}

#[derive(Debug, Clone)]
pub struct CommandPlan {
    parsed: ParsedInputCmd,
    consistency: super::session_runtime::CompletionConsistency,
}

impl CommandPlan {
    pub fn from_parsed(parsed: ParsedInputCmd) -> Result<Self> {
        if parsed.prefix.is_empty() {
            return Err(Error::InvalidPrefix("prefix cannot be empty".to_string()).into());
        }
        Ok(Self {
            parsed: parsed.with_default_target(Target::Broadcast),
            consistency: super::session_runtime::CompletionConsistency::StateConsistent,
        })
    }

    pub fn protocol_complete(mut self) -> Self {
        self.consistency = super::session_runtime::CompletionConsistency::ProtocolComplete;
        self
    }

    pub fn target(mut self, target: Target) -> Self {
        self.parsed.target = target;
        self
    }

    fn into_parts(self, internal_token: u64) -> (Target, Command) {
        let (target, command) = self.parsed.to_command(internal_token);
        (target, command.with_consistency(self.consistency))
    }
}

pub fn command(command: &str) -> Result<CommandPlan> {
    let parsed: ParsedInputCmd = command
        .try_into()
        .with_context(|| format!("Failed to parse command: {}", command))?;
    CommandPlan::from_parsed(parsed)
}

pub fn parsed(command: ParsedInputCmd) -> Result<CommandPlan> {
    CommandPlan::from_parsed(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_preserves_embedded_target_and_external_token() {
        let request = command("42-thread-info --all").unwrap();
        assert_eq!(request.parsed.external_token, Some(42));
        assert_eq!(request.parsed.target, Target::Broadcast);
    }

    #[test]
    fn explicit_target_replaces_parsed_target() {
        let request = command("-thread-info --all")
            .unwrap()
            .target(Target::Session(7));
        assert_eq!(request.parsed.target, Target::Session(7));
    }

    #[test]
    fn empty_prefix_is_rejected() {
        let parsed = ParsedInputCmd {
            external_token: None,
            prefix: String::new(),
            args: String::new(),
            target: Target::Broadcast,
        };
        assert!(CommandPlan::from_parsed(parsed).is_err());
    }
}
