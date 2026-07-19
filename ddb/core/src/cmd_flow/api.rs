//! Semantic command execution API.
//!
//! A request carries parsed debugger command data and an explicit target. Its
//! terminal operation states the caller's intent: `execute` awaits responses,
//! `emit` formats responses asynchronously, and `submit` deliberately discards
//! them after admission. Transport channels and correlation plumbing never escape.

use anyhow::{Context, Result};

use super::{
    get_router,
    input::{Command, ParsedInputCmd},
    DynFormatter, FinishedCmd, PlainFormatter,
};

pub use super::router::Target;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid command prefix: {0}")]
    InvalidPrefix(String),
    #[error("Command execution failed: {0}")]
    Execution(#[from] anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    parsed: ParsedInputCmd,
    consistency: super::session_runtime::CompletionConsistency,
}

impl ExecutionRequest {
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

    pub fn state_consistent(mut self) -> Self {
        self.consistency = super::session_runtime::CompletionConsistency::StateConsistent;
        self
    }

    pub fn target(mut self, target: Target) -> Self {
        self.parsed.target = target;
        self
    }

    pub fn target_or_default(self, target: Option<Target>) -> Self {
        match target {
            Some(target) => self.target(target),
            None => self,
        }
    }

    fn into_parts(self) -> (Target, Command) {
        let (target, command) = self.parsed.to_command();
        (target, command.with_consistency(self.consistency))
    }

    pub async fn execute(self) -> Result<FinishedCmd> {
        let (target, command) = self.into_parts();
        get_router().execute(target, command).await
    }

    pub(crate) async fn execute_exclusive(
        self,
        lease: &super::session_runtime::SessionLease,
    ) -> Result<FinishedCmd> {
        let (target, command) = self.into_parts();
        get_router().execute_exclusive(lease, target, command).await
    }

    pub(crate) async fn execute_with_optional_lease(
        self,
        lease: Option<&super::session_runtime::SessionLease>,
    ) -> Result<FinishedCmd> {
        match lease {
            Some(lease) => self.execute_exclusive(lease).await,
            None => self.execute().await,
        }
    }

    pub(crate) async fn emit_exclusive<F>(
        self,
        lease: &super::session_runtime::SessionLease,
        formatter: F,
    ) -> Result<()>
    where
        F: DynFormatter + 'static,
    {
        let finished = self.execute_exclusive(lease).await?;
        super::emit(finished, Box::new(formatter));
        Ok(())
    }

    pub async fn emit<F>(self, formatter: F) -> Result<()>
    where
        F: DynFormatter + 'static,
    {
        let (target, command) = self.into_parts();
        get_router().emit(target, command, formatter).await
    }

    pub async fn emit_plain(self) -> Result<()> {
        self.emit(PlainFormatter).await
    }

    pub async fn submit(self) -> Result<()> {
        let (target, command) = self.into_parts();
        get_router().submit(target, command).await
    }
}

pub fn command(command: &str) -> Result<ExecutionRequest> {
    let parsed: ParsedInputCmd = command
        .try_into()
        .with_context(|| format!("Failed to parse command: {}", command))?;
    ExecutionRequest::from_parsed(parsed)
}

pub fn parsed(command: ParsedInputCmd) -> Result<ExecutionRequest> {
    ExecutionRequest::from_parsed(command)
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
            internal_token: 1,
            prefix: String::new(),
            args: String::new(),
            target: Target::Broadcast,
        };
        assert!(ExecutionRequest::from_parsed(parsed).is_err());
    }
}
