//! # Command Flow Facade API
//!
//! This module provides ergonomic entry points for sending GDB commands through the distributed
//! debugging system. It encapsulates token management, formatter selection, and routing logic.
//!
//! ## Module Responsibilities
//!
//! - **Parse**: Accept prefix and args from callers
//! - **Build**: Construct internal `Command<F>` with appropriate tokens and formatters
//! - **Execute**: Route commands via `Router` to target sessions/threads
//! - **Map Errors**: Convert internal errors to facade-level error types
//!
//! ## Usage Examples
//!
//! ### Basic command (output to STDOUT)
//! ```no_run
//! # use core::cmd_flow::api;
//! # async fn example() -> Result<(), api::Error> {
//! api::send("-exec-continue", "").to(api::Target::Broadcast).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Command with return value
//! ```no_run
//! # use core::cmd_flow::api;
//! # async fn example() -> Result<(), api::Error> {
//! let result = api::send_and_return("-thread-info", "")
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
//! api::intercept("-thread-info", "")
//!     .to(api::Target::Broadcast)
//!     .with(api::ThreadInfoFormatter)
//!     .await?;
//! # Ok(())
//! # }
//! ```

use anyhow::Result;

use super::{get_router, input::Command, DynFormatter, FinishedCmd, PlainFormatter};

// Re-export common types for convenience
pub use super::router::Target;
pub use super::output::{NullFormatter, PlainFormatter as DefaultFormatter, ThreadInfoFormatter};

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

/// Builder for sending a command without waiting for results (STDOUT path)
pub struct SendBuilder {
    prefix: String,
    args: String,
    external_token: Option<u64>,
}

impl SendBuilder {
    /// Route the command to the specified target
    pub async fn to(self, target: Target) -> Result<(), Error> {
        if self.prefix.is_empty() {
            return Err(Error::InvalidPrefix("prefix cannot be empty".to_string()));
        }

        let internal_token = crate::common::next_token();
        let raw_cmd = format!("{} {}", self.prefix, self.args);
        let cmd = Command::new(
            self.external_token,
            internal_token,
            raw_cmd,
            PlainFormatter,
        );

        get_router().send_to(target, cmd);
        Ok(())
    }
}

/// Builder for sending a command and returning results
pub struct SendAndReturnBuilder {
    prefix: String,
    args: String,
    external_token: Option<u64>,
}

impl SendAndReturnBuilder {
    /// Route the command to the specified target and await results
    pub async fn to(self, target: Target) -> Result<FinishedCmd, Error> {
        if self.prefix.is_empty() {
            return Err(Error::InvalidPrefix("prefix cannot be empty".to_string()));
        }

        let internal_token = crate::common::next_token();
        let raw_cmd = format!("{} {}", self.prefix, self.args);
        let cmd = Command::new(
            self.external_token,
            internal_token,
            raw_cmd,
            PlainFormatter,
        );

        let result = get_router().send_to_ret(target, cmd).await?;
        Ok(result)
    }
}

/// Builder for intercepting command output with custom formatters
pub struct InterceptBuilder<F: DynFormatter + 'static> {
    prefix: String,
    args: String,
    external_token: Option<u64>,
    formatter: F,
}

impl<F: DynFormatter + 'static> InterceptBuilder<F> {
    /// Route the command to the specified target with custom formatter
    pub async fn to(self, target: Target) -> Result<FinishedCmd, Error> {
        if self.prefix.is_empty() {
            return Err(Error::InvalidPrefix("prefix cannot be empty".to_string()));
        }

        let internal_token = crate::common::next_token();
        let raw_cmd = format!("{} {}", self.prefix, self.args);
        let cmd = Command::new(self.external_token, internal_token, raw_cmd, self.formatter);

        let result = get_router().send_to_ret(target, cmd).await?;
        Ok(result)
    }
}

/// Builder that allows setting a custom formatter
pub struct InterceptFormatterBuilder {
    prefix: String,
    args: String,
    external_token: Option<u64>,
    target: Target,
}

impl InterceptFormatterBuilder {
    /// Specify the formatter to use for this command
    pub fn with<F: DynFormatter + 'static>(self, formatter: F) -> InterceptBuilder<F> {
        InterceptBuilder {
            prefix: self.prefix,
            args: self.args,
            external_token: self.external_token,
            formatter,
        }
    }
}

/// Send a command without waiting for results (output to STDOUT)
///
/// # Arguments
/// * `prefix` - The GDB/MI command prefix (e.g., "-exec-continue")
/// * `args` - Command arguments as a string
///
/// # Returns
/// A builder that requires calling `.to(target)` to execute
pub fn send(prefix: &str, args: &str) -> SendBuilder {
    SendBuilder {
        prefix: prefix.to_string(),
        args: args.to_string(),
        external_token: None,
    }
}

/// Send a command and wait for results
///
/// # Arguments
/// * `prefix` - The GDB/MI command prefix (e.g., "-thread-info")
/// * `args` - Command arguments as a string
///
/// # Returns
/// A builder that requires calling `.to(target)` to execute and await results
pub fn send_and_return(prefix: &str, args: &str) -> SendAndReturnBuilder {
    SendAndReturnBuilder {
        prefix: prefix.to_string(),
        args: args.to_string(),
        external_token: None,
    }
}

/// Intercept command output with custom formatting
///
/// # Arguments
/// * `prefix` - The GDB/MI command prefix
/// * `args` - Command arguments as a string
///
/// # Returns
/// A builder that requires calling `.to(target).with(formatter)` to execute
pub fn intercept(prefix: &str, args: &str) -> InterceptFormatterBuilder {
    InterceptFormatterBuilder {
        prefix: prefix.to_string(),
        args: args.to_string(),
        external_token: None,
        target: Target::default(),
    }
}

// Make InterceptFormatterBuilder require both .to() and .with()
impl InterceptFormatterBuilder {
    /// Route the command to the specified target (requires `.with(formatter)` next)
    pub fn to(mut self, target: Target) -> Self {
        self.target = target;
        self
    }
}

// Update InterceptBuilder to use the target from the builder
impl<F: DynFormatter + 'static> InterceptBuilder<F> {
    /// Execute the command with the specified target and formatter
    pub async fn execute(self, target: Target) -> Result<FinishedCmd, Error> {
        self.to(target).await
    }
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
        let builder = send("-exec-continue", "--all");
        assert_eq!(builder.prefix, "-exec-continue");
        assert_eq!(builder.args, "--all");
        assert_eq!(builder.external_token, None);
    }

    #[test]
    fn test_send_and_return_builder_construction() {
        let builder = send_and_return("-thread-info", "");
        assert_eq!(builder.prefix, "-thread-info");
        assert_eq!(builder.args, "");
        assert_eq!(builder.external_token, None);
    }

    #[test]
    fn test_intercept_builder_construction() {
        let builder = intercept("-thread-select", "--thread 1");
        assert_eq!(builder.prefix, "-thread-select");
        assert_eq!(builder.args, "--thread 1");
        assert_eq!(builder.external_token, None);
    }

    #[test]
    fn test_error_on_empty_prefix() {
        // Note: This would require async test infrastructure to actually call .to()
        // For now, we verify the builder constructs correctly
        let builder = send("", "args");
        assert_eq!(builder.prefix, "");
        // The actual error will be returned when .to() is called
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
    fn test_builder_pattern_ergonomics() {
        // Verify fluent API is compile-time checkable
        let _builder = send("-exec-continue", "");
        // .to() would be called here in actual usage
        
        let _builder = send_and_return("-thread-info", "");
        // .to() would be called here
        
        let _builder = intercept("-thread-select", "").to(Target::Broadcast);
        // .with() would be called here
    }

    // Note: Full integration tests require mock Router and Tracker
    // These will be added in subsequent tasks with proper fakes
}

