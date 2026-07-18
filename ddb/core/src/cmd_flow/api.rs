//! # Command Flow Facade API
//!
//! This module provides ergonomic entry points for sending GDB commands through the distributed
//! debugging system. It encapsulates token management, formatter selection, and routing logic.
//!
//! ## Usage Examples
//!
//! ### Basic command (output to STDOUT)
//! ```no_run
//! # use core::cmd_flow::api;
//! # async fn example() -> Result<(), api::Error> {
//! api::send("-exec-continue").to(api::Target::Broadcast).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Command with return value
//! ```no_run
//! # use core::cmd_flow::api;
//! # async fn example() -> Result<(), api::Error> {
//! let result = api::send_and_return("-thread-info")
//!     .to(api::Target::CurrSession)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Command with custom formatter (fire-and-forget)
//! ```no_run
//! # use core::cmd_flow::api;
//! # fn example() -> Result<(), api::Error> {
//! api::send("-thread-info")?
//!     .with(api::ThreadInfoFormatter)
//!     .to(api::Target::Broadcast)?;
//! # Ok(())
//! # }
//! ```
//!
//!

use anyhow::{Context, Result};

use crate::cmd_flow::input::ParsedInputCmd;

use super::{get_router, input::Command, DynFormatter, FinishedCmd, PlainFormatter};

// Re-export common types for convenience
#[allow(unused_imports)]
pub use super::output::{NullFormatter, PlainFormatter as DefaultFormatter, ThreadInfoFormatter};
pub use super::router::Target;

/// Facade-level error type for command flow operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid command prefix: {0}")]
    InvalidPrefix(String),

    #[error("Target resolution failed: {0}")]
    TargetResolution(String),

    #[error("Command execution failed: {0}")]
    ExecutionError(#[from] anyhow::Error),
}

#[inline]
fn check_prefix(parsed_cmd: &ParsedInputCmd) -> Result<()> {
    if parsed_cmd.prefix.is_empty() {
        return Err(Error::InvalidPrefix("prefix cannot be empty".to_string()).into());
    }
    Ok(())
}

#[inline]
fn prepare_to_send<F: DynFormatter + 'static + Clone>(
    parsed_cmd: ParsedInputCmd,
    formatter: F,
) -> Result<(Command<F>, Target)> {
    check_prefix(&parsed_cmd)?;
    let (cmd_to_send, target) = Command::new_with_parsed_cmd(parsed_cmd, formatter);
    Ok((cmd_to_send, target))
}

/// Builder for sending a command without waiting for results (STDOUT path)
pub struct SendBuilder {
    parsed_cmd: ParsedInputCmd,
}

impl SendBuilder {
    /// Specify a custom formatter for this command
    pub fn with<F: DynFormatter + 'static>(self, formatter: F) -> SendWithFormatterBuilder<F> {
        SendWithFormatterBuilder {
            parsed_cmd: self.parsed_cmd,
            formatter,
        }
    }

    /// Route the command to the specified target
    pub fn to<const SUPPRESS_OUTPUT: bool>(self, target: Target) -> Result<()> {
        match SUPPRESS_OUTPUT {
            true => {
                let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, NullFormatter)?;
                get_router().send_to(target, cmd_to_send)?;
            }
            false => {
                let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
                get_router().send_to(target, cmd_to_send)?;
            }
        }
        Ok(())
    }

    /// Route the command to the target specified in the command itself.
    /// If the command does not specify a target, it defaults to the default target.
    pub fn to_default_target<const SUPPRESS_OUTPUT: bool>(self) -> Result<()> {
        if SUPPRESS_OUTPUT {
            let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, NullFormatter)?;
            get_router().send_to(target, cmd_to_send)?;
        } else {
            let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
            get_router().send_to(target, cmd_to_send)?;
        }
        Ok(())
    }

    /// Route the command to the specified target or, if not specified, to the target
    /// in the original command (fire-and-forget).
    /// Does not await or return results.
    pub fn to_or_default<const SUPPRESS_OUTPUT: bool>(self, target: Option<Target>) -> Result<()> {
        if let Some(target) = target {
            self.to::<SUPPRESS_OUTPUT>(target)
        } else {
            self.to_default_target::<SUPPRESS_OUTPUT>()
        }
    }
}

/// Builder for sending a command by discarding results directly (DISCARD path)
pub struct SendAndForgetBuilder {
    parsed_cmd: ParsedInputCmd,
}

impl SendAndForgetBuilder {
    /// Route the command to the specified target
    pub fn to(self, target: Target) -> Result<()> {
        let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, NullFormatter)?;
        get_router().send_to_and_forget(target, cmd_to_send)?;
        Ok(())
    }

    /// Route the command to the target specified in the command itself.
    /// If the command does not specify a target, it defaults to the default target.
    pub fn to_default_target(self) -> Result<()> {
        let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, NullFormatter)?;
        get_router().send_to_and_forget(target, cmd_to_send)?;
        Ok(())
    }

    /// Route the command to the specified target or, if not specified,
    /// to the target in the original command.
    pub fn to_or_default(self, target: Option<Target>) -> Result<()> {
        if let Some(target) = target {
            self.to(target)
        } else {
            self.to_default_target()
        }
    }
}

/// Builder for sending a command and returning results
pub struct SendAndReturnBuilder {
    parsed_cmd: ParsedInputCmd,
}

impl SendAndReturnBuilder {
    /// Route the command to the specified target and await results
    pub async fn to(self, target: Target) -> Result<FinishedCmd> {
        // ignore command specified target, as caller specified one.
        let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
        let result = get_router().send_to_ret(target, cmd_to_send).await?;
        Ok(result)
    }

    /// Route the command to the specified target and await results
    /// If the target is not specified, it defaults to the target
    /// specified in the original command.
    pub async fn to_or_default(self, target: Option<Target>) -> Result<FinishedCmd> {
        if let Some(target) = target {
            self.to(target).await
        } else {
            self.to_default_target().await
        }
    }

    /// Route the command to the target specified in the command itself.
    /// If the command does not specify a target, it defaults to the default target.
    pub async fn to_default_target(self) -> Result<FinishedCmd> {
        let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
        let result = get_router().send_to_ret(target, cmd_to_send).await?;
        Ok(result)
    }
}

/// Builder for sending a command with custom formatter without waiting for result
pub struct SendWithFormatterBuilder<F: DynFormatter + 'static> {
    parsed_cmd: ParsedInputCmd,
    formatter: F,
}

impl<F: DynFormatter + 'static + Clone> SendWithFormatterBuilder<F> {
    /// Route the command to the specified target with custom formatter (fire-and-forget)
    pub fn to(self, target: Target) -> Result<()> {
        let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, self.formatter)?;
        get_router().send_to(target, cmd_to_send)?;
        Ok(())
    }

    /// Route the command to the specified target or default target (fire-and-forget)
    /// If the target is not specified, it defaults to the target specified in the original command.
    pub fn to_or_default(self, target: Option<Target>) -> Result<()> {
        if let Some(target) = target {
            self.to(target)
        } else {
            self.to_default_target()
        }
    }

    /// Route the command to the target specified in the command itself (fire-and-forget)
    /// If the command does not specify a target, it defaults to the default target.
    pub fn to_default_target(self) -> Result<()> {
        let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, self.formatter)?;
        get_router().send_to(target, cmd_to_send)?;
        Ok(())
    }
}

impl Into<SendBuilder> for ParsedInputCmd {
    fn into(self) -> SendBuilder {
        SendBuilder { parsed_cmd: self }
    }
}

impl Into<SendAndReturnBuilder> for ParsedInputCmd {
    fn into(self) -> SendAndReturnBuilder {
        SendAndReturnBuilder { parsed_cmd: self }
    }
}

impl Into<SendAndForgetBuilder> for ParsedInputCmd {
    fn into(self) -> SendAndForgetBuilder {
        SendAndForgetBuilder { parsed_cmd: self }
    }
}

/// Send a command without waiting for results (output to STDOUT directly)
///
/// # Arguments
/// * `command` - The GDB/MI command (e.g., "-exec-continue" or "-thread-info --thread 1" or "38-thread-info")
///
/// # Returns
/// A builder that requires calling `.to(target)` to execute
pub fn send(command: &str) -> Result<SendBuilder> {
    let parsed_cmd: ParsedInputCmd = command
        .try_into()
        .context(format!("Failed to parse command: {}", command))?;
    Ok(parsed_cmd.into())
}

/// Send a command by discarding results (DISCARD path)
///
/// # Arguments
/// * `command` - The GDB/MI command (e.g., "-exec-continue" or "-thread-info --thread 1" or "38-thread-info")
///
/// # Returns
/// A builder that requires calling `.to(target)` to execute
///
/// # Note
/// This function is different from `send` in that it discards any output from the command.
/// `send` routes the command and outputs results to STDOUT directly (after applying formatter).
/// Both `send` and `send_and_forget` are fire-and-forget; they do not await results.
pub fn send_and_forget(command: &str) -> Result<SendAndForgetBuilder> {
    let parsed_cmd: ParsedInputCmd = command
        .try_into()
        .context(format!("Failed to parse command: {}", command))?;
    Ok(parsed_cmd.into())
}

/// Send a command and wait for results.
///
/// **Note:** caller should decide how to handle output.
///
/// # Arguments
/// * `command` - The GDB/MI command (e.g., "-exec-continue" or "-thread-info --thread 1" or "38-thread-info")
///
/// # Returns
/// A builder that requires calling `.to(target)` to execute and await results
pub fn send_and_return(command: &str) -> Result<SendAndReturnBuilder> {
    let parsed_cmd: ParsedInputCmd = command
        .try_into()
        .context(format!("Failed to parse command: {}", command))?;
    Ok(parsed_cmd.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_types() {
        let err = Error::InvalidPrefix("test".to_string());
        assert!(err.to_string().contains("Invalid command prefix"));

        let err = Error::TargetResolution("no session".to_string());
        assert!(err.to_string().contains("Target resolution failed"));
    }

    #[test]
    fn test_send_builder_construction() {
        let builder = send("-exec-continue --all").unwrap();
        assert_eq!(builder.parsed_cmd.prefix, "-exec-continue");
        // "--all" will be stripped out and converted to "BROADCAST" target.
        assert_eq!(builder.parsed_cmd.args, "");
        assert_eq!(builder.parsed_cmd.external_token, None);
        assert_eq!(builder.parsed_cmd.target, Target::Broadcast);
    }

    #[test]
    fn test_send_and_return_builder_construction() {
        let builder = send_and_return("-thread-info").unwrap();
        assert_eq!(builder.parsed_cmd.prefix, "-thread-info");
        assert_eq!(builder.parsed_cmd.args, "");
        assert_eq!(builder.parsed_cmd.external_token, None);
        assert_eq!(builder.parsed_cmd.target, Target::default());
    }

    #[test]
    fn test_error_on_empty_prefix() {
        let _builder = send("args");
        // invalid command
        assert_eq!(_builder.is_err(), true);
    }

    #[test]
    fn test_target_re_exports() {
        // Verify Target enum is re-exported
        let _target = Target::Broadcast;
        let _target = Target::CurrSession;
        let _target = Target::CurrThread;
        let _target = Target::Session(1);
        let _target = Target::Thread(1);
    }

    #[test]
    fn test_formatter_re_exports() {
        // Verify formatter types are re-exported
        let _formatter = DefaultFormatter;
        let _formatter = NullFormatter;
        let _formatter = ThreadInfoFormatter;
    }

    #[test]
    fn test_parse_ext_token() {
        let builder = send("42-exec-continue --all").unwrap();
        assert_eq!(builder.parsed_cmd.prefix, "-exec-continue");
        // "--all" will be stripped out and converted to "BROADCAST" target.
        assert_eq!(builder.parsed_cmd.args, "");
        assert_eq!(builder.parsed_cmd.external_token, Some(42));
        assert_eq!(builder.parsed_cmd.target, Target::Broadcast);
    }

    #[test]
    fn test_send_with_custom_formatter() {
        // Test that send().with() can be constructed with a custom formatter
        let builder = send("-thread-info").unwrap().with(ThreadInfoFormatter);

        assert_eq!(builder.parsed_cmd.prefix, "-thread-info");
        assert_eq!(builder.parsed_cmd.args, "");
        assert_eq!(builder.parsed_cmd.external_token, None);
    }

    #[test]
    fn test_send_with_formatter_preserves_external_token() {
        // Test that send().with() preserves external token
        let builder = send("42-thread-info").unwrap().with(NullFormatter);

        assert_eq!(builder.parsed_cmd.prefix, "-thread-info");
        assert_eq!(builder.parsed_cmd.external_token, Some(42));
    }
}
