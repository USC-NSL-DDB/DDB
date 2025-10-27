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
//! ### Command with custom formatter
//! ```no_run
//! # use core::cmd_flow::api;
//! # async fn example() -> Result<(), api::Error> {
//! api::intercept("-thread-info")
//!     .with(api::ThreadInfoFormatter)
//!     .to(api::Target::Broadcast)
//!     .await?;
//! # Ok(())
//! # }
//! ```

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
fn check_prefix(parsed_cmd: &ParsedInputCmd) -> Result<(), Error> {
    if parsed_cmd.prefix.is_empty() {
        return Err(Error::InvalidPrefix("prefix cannot be empty".to_string()));
    }
    Ok(())
}

#[inline]
fn prepare_to_send<F: DynFormatter + 'static>(
    parsed_cmd: ParsedInputCmd,
    formatter: F,
) -> Result<(Command<F>, Target), Error> {
    check_prefix(&parsed_cmd)?;
    let (cmd_to_send, target) = Command::new_with_parsed_cmd(parsed_cmd, formatter);
    Ok((cmd_to_send, target))
}

/// Builder for sending a command without waiting for results (STDOUT path)
pub struct SendBuilder {
    parsed_cmd: ParsedInputCmd,
}

impl SendBuilder {
    /// Route the command to the specified target
    pub async fn to(self, target: Target) -> Result<(), Error> {
        let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
        get_router().send_to(target, cmd_to_send);
        Ok(())
    }

    /// Route the command to the target specified in the command itself.
    /// If the command does not specify a target, it defaults to the default target.
    pub async fn to_default_target(self) -> Result<(), Error> {
        let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
        get_router().send_to(target, cmd_to_send);
        Ok(())
    }
}

/// Builder for sending a command and returning results
pub struct SendAndReturnBuilder {
    parsed_cmd: ParsedInputCmd,
}

impl SendAndReturnBuilder {
    /// Route the command to the specified target and await results
    pub async fn to(self, target: Target) -> Result<FinishedCmd, Error> {
        // ignore command specified target, as caller specified one.
        let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
        let result = get_router().send_to_ret(target, cmd_to_send).await?;
        Ok(result)
    }

    /// Route the command to the target specified in the command itself.
    /// If the command does not specify a target, it defaults to the default target.
    pub async fn to_default_target(self) -> Result<FinishedCmd, Error> {
        let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, PlainFormatter)?;
        let result = get_router().send_to_ret(target, cmd_to_send).await?;
        Ok(result)
    }
}

/// Builder for intercepting command output with custom formatters
pub struct InterceptBuilder<F: DynFormatter + 'static> {
    parsed_cmd: ParsedInputCmd,
    formatter: F,
}

impl<F: DynFormatter + 'static> InterceptBuilder<F> {
    /// Route the command to the specified target with custom formatter
    pub async fn to(self, target: Target) -> Result<FinishedCmd, Error> {
        // ignore command specified target, as caller specified one.
        let (cmd_to_send, _) = prepare_to_send(self.parsed_cmd, self.formatter)?;
        let result = get_router().send_to_ret(target, cmd_to_send).await?;
        Ok(result)
    }

    /// Route the command to the target specified in the command itself.
    /// If the command does not specify a target, it defaults to the default target.
    pub async fn to_default_target(self) -> Result<FinishedCmd, Error> {
        let (cmd_to_send, target) = prepare_to_send(self.parsed_cmd, self.formatter)?;
        let result = get_router().send_to_ret(target, cmd_to_send).await?;
        Ok(result)
    }
}

/// Builder that allows setting a custom formatter
pub struct InterceptFormatterBuilder {
    parsed_cmd: ParsedInputCmd,
}

impl InterceptFormatterBuilder {
    /// Specify the formatter to use for this command
    pub fn with<F: DynFormatter + 'static>(self, formatter: F) -> InterceptBuilder<F> {
        InterceptBuilder {
            parsed_cmd: self.parsed_cmd,
            formatter,
        }
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
    Ok(SendBuilder { parsed_cmd })
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
    Ok(SendAndReturnBuilder { parsed_cmd })
}

/// Intercept command output with custom formatting
///
/// # Arguments
/// * `command` - The GDB/MI command (e.g., "-exec-continue" or "-thread-info --thread 1" or "38-thread-info")
///
/// # Returns
/// A builder that requires calling `.with(formatter).to(target)` to execute
pub fn intercept(command: &str) -> Result<InterceptFormatterBuilder> {
    let parsed_cmd: ParsedInputCmd = command
        .try_into()
        .context(format!("Failed to parse command: {}", command))?;
    Ok(InterceptFormatterBuilder { parsed_cmd })
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
}
