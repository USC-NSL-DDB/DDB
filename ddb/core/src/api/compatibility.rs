//! Frozen v1/legacy command translation over the shared command engine.
//!
//! Compatibility transports own HTTP shapes, but backend command construction
//! belongs here so Axum handlers remain framing adapters. New frontends should
//! use the typed v2 application service instead of extending this module.

use std::sync::Arc;

use anyhow::Result;
use serde_json::json;

use super::contract::{
    ApiError, ApiTarget, BreakpointCreateRequest, BreakpointUpdateRequest, CommandCompletion,
    CommandReceipt, CommandReceiptState, CommandRequest, DistributedBacktraceRequest,
    EvaluateRequest, ExecutionAction, ExecutionRequest, MemoryReadRequest, StackFramesRequest,
    StackVariablesRequest, ThreadQueryRequest, ThreadSelectRequest, VariableValues,
};
use crate::{
    cmd_flow::{
        breakpoint::breakpoint_insert_command,
        engine::{CommandEngine, CommandError},
        router::Target,
        FinishedCmd,
    },
    state::{BkptLoc, BreakpointProperties},
};

pub(crate) struct CompatibilityCommandResponse {
    pub(crate) accepted: bool,
    pub(crate) receipt: CommandReceipt,
}

/// Stable v1 command behavior isolated from transport framing.
pub(crate) struct CompatibilityCommandService {
    engine: Arc<CommandEngine>,
}

impl CompatibilityCommandService {
    pub(crate) fn new(engine: Arc<CommandEngine>) -> Arc<Self> {
        Arc::new(Self { engine })
    }

    pub(crate) async fn execute_command(
        &self,
        request: CommandRequest,
    ) -> Result<CompatibilityCommandResponse, ApiError> {
        if request.command.trim().is_empty() {
            return Err(ApiError::bad_request(
                "empty_command",
                "command must not be empty",
            ));
        }
        let target = request.target.map(Target::try_from).transpose()?;
        if request.wait {
            let result = self
                .engine
                .execute_api(&request.command, target)
                .await
                .map_err(command_failed)?
                .into_response();
            Ok(CompatibilityCommandResponse {
                accepted: false,
                receipt: completed_receipt(result.as_ref()),
            })
        } else {
            self.engine
                .submit_api(&request.command, target)
                .await
                .map_err(|error| {
                    ApiError::unprocessable("command_rejected", error.to_string())
                        .with_details(json!({"external_token": error.external_token()}))
                })?;
            Ok(CompatibilityCommandResponse {
                accepted: true,
                receipt: CommandReceipt {
                    state: CommandReceiptState::Accepted,
                    result: None,
                },
            })
        }
    }

    pub(crate) async fn execute_control(
        &self,
        request: ExecutionRequest,
    ) -> Result<CommandReceipt, ApiError> {
        let command = match request.action {
            ExecutionAction::Continue => "-record-time-and-continue".to_string(),
            ExecutionAction::Interrupt => "-exec-interrupt".to_string(),
            ExecutionAction::Next => "-record-time-and-next".to_string(),
            ExecutionAction::StepIn => "-record-time-and-step".to_string(),
            ExecutionAction::StepOut => "-record-time-and-finish".to_string(),
            ExecutionAction::Jump => format!(
                "-exec-jump {}",
                quote_required(request.location.as_deref(), "location")?
            ),
            ExecutionAction::SendSignal => format!(
                "-send-signal {}",
                quote_required(request.signal.as_deref(), "signal")?
            ),
        };
        self.waited_command(command, Some(request.target)).await
    }

    pub(crate) async fn query_threads(
        &self,
        request: ThreadQueryRequest,
    ) -> Result<CommandReceipt, ApiError> {
        self.waited_command("-thread-info".to_string(), request.target)
            .await
    }

    pub(crate) async fn select_thread(
        &self,
        request: ThreadSelectRequest,
    ) -> Result<CommandReceipt, ApiError> {
        self.waited_command(
            format!("-thread-select {}", request.thread_id),
            Some(ApiTarget::Thread {
                thread_id: request.thread_id,
            }),
        )
        .await
    }

    pub(crate) async fn stack_frames(
        &self,
        request: StackFramesRequest,
    ) -> Result<CommandReceipt, ApiError> {
        if request
            .low
            .zip(request.high)
            .is_some_and(|(low, high)| low > high)
        {
            return Err(ApiError::bad_request(
                "invalid_frame_range",
                "low must be less than or equal to high",
            ));
        }
        let range = match (request.low, request.high) {
            (Some(low), Some(high)) => format!(" {low} {high}"),
            (Some(low), None) => format!(" {low}"),
            (None, Some(_)) => {
                return Err(ApiError::bad_request(
                    "invalid_frame_range",
                    "high requires low",
                ))
            }
            (None, None) => String::new(),
        };
        self.waited_command(
            format!("-stack-list-frames{range}"),
            Some(ApiTarget::Thread {
                thread_id: request.thread_id,
            }),
        )
        .await
    }

    pub(crate) async fn stack_variables(
        &self,
        request: StackVariablesRequest,
    ) -> Result<CommandReceipt, ApiError> {
        let mut command = "-stack-list-variables".to_string();
        if let Some(frame) = request.frame {
            command.push_str(&format!(" --frame {frame}"));
        }
        command.push_str(match request.values {
            VariableValues::None => " --no-values",
            VariableValues::Simple => " --simple-values",
            VariableValues::All => " --all-values",
        });
        self.waited_command(
            command,
            Some(ApiTarget::Thread {
                thread_id: request.thread_id,
            }),
        )
        .await
    }

    pub(crate) async fn evaluate(
        &self,
        request: EvaluateRequest,
    ) -> Result<CommandReceipt, ApiError> {
        if request.expression.trim().is_empty() {
            return Err(ApiError::bad_request(
                "empty_expression",
                "expression must not be empty",
            ));
        }
        let frame = request
            .frame
            .map(|frame| format!(" --frame {frame}"))
            .unwrap_or_default();
        self.waited_command(
            format!(
                "-data-evaluate-expression{frame} {}",
                quote(&request.expression)
            ),
            Some(request.target),
        )
        .await
    }

    pub(crate) async fn read_memory(
        &self,
        request: MemoryReadRequest,
    ) -> Result<CommandReceipt, ApiError> {
        if request.count == 0 || request.count > 1024 * 1024 {
            return Err(ApiError::bad_request(
                "invalid_memory_count",
                "count must be between 1 and 1048576 bytes",
            ));
        }
        if request.address.trim().is_empty() {
            return Err(ApiError::bad_request(
                "empty_address",
                "address must not be empty",
            ));
        }
        self.waited_command(
            memory_read_command(&request.address, request.count, request.offset),
            Some(request.target),
        )
        .await
    }

    pub(crate) async fn create_breakpoint(
        &self,
        request: BreakpointCreateRequest,
    ) -> Result<CommandReceipt, ApiError> {
        if request.source.trim().is_empty() || request.line == 0 {
            return Err(ApiError::bad_request(
                "invalid_breakpoint_location",
                "source must not be empty and line must be greater than zero",
            ));
        }
        let location = BkptLoc::new(request.source, request.line);
        let properties = BreakpointProperties {
            enabled: true,
            condition: request.condition,
            temporary: request.temporary,
            hardware: request.hardware,
        };
        self.waited_command(
            breakpoint_insert_command(&location, &properties),
            Some(request.target),
        )
        .await
    }

    pub(crate) async fn delete_breakpoint(
        &self,
        breakpoint_id: u64,
    ) -> Result<CommandReceipt, ApiError> {
        self.waited_command(format!("-break-delete {breakpoint_id}"), None)
            .await
    }

    pub(crate) async fn update_breakpoint(
        &self,
        breakpoint_id: u64,
        request: BreakpointUpdateRequest,
    ) -> Result<CommandReceipt, ApiError> {
        self.waited_command(
            format!(
                "{} {breakpoint_id}",
                if request.enabled {
                    "-break-enable"
                } else {
                    "-break-disable"
                }
            ),
            None,
        )
        .await
    }

    pub(crate) async fn distributed_backtrace(
        &self,
        request: DistributedBacktraceRequest,
    ) -> Result<CommandReceipt, ApiError> {
        self.waited_command(
            "-bt-remote".to_string(),
            Some(ApiTarget::Thread {
                thread_id: request.thread_id,
            }),
        )
        .await
    }

    pub(crate) async fn execute_legacy(
        &self,
        command: &str,
        target: Option<Target>,
        wait: bool,
    ) -> Result<Option<FinishedCmd>> {
        if wait {
            Ok(self
                .engine
                .execute_api(command, target)
                .await?
                .into_response())
        } else {
            self.engine.submit_api(command, target).await?;
            Ok(None)
        }
    }

    async fn waited_command(
        &self,
        command: String,
        target: Option<ApiTarget>,
    ) -> Result<CommandReceipt, ApiError> {
        let result = self.run_command(&command, target).await?;
        Ok(completed_receipt(result.as_ref()))
    }

    async fn run_command(
        &self,
        command: &str,
        target: Option<ApiTarget>,
    ) -> Result<Option<FinishedCmd>, ApiError> {
        if command.trim().is_empty() {
            return Err(ApiError::bad_request(
                "empty_command",
                "command must not be empty",
            ));
        }
        let target = target.map(Target::try_from).transpose()?;
        self.engine
            .execute_api(command, target)
            .await
            .map(|outcome| outcome.into_response())
            .map_err(command_failed)
    }
}

fn completed_receipt(result: Option<&FinishedCmd>) -> CommandReceipt {
    CommandReceipt {
        state: CommandReceiptState::Completed,
        result: result.map(CommandCompletion::from),
    }
}

fn command_failed(error: CommandError) -> ApiError {
    ApiError::unprocessable("command_failed", error.to_string())
        .with_details(json!({"external_token": error.external_token()}))
}

fn memory_read_command(address: &str, count: u64, offset: Option<i64>) -> String {
    let offset = offset
        .map(|offset| format!(" -o {offset}"))
        .unwrap_or_default();
    format!("-data-read-memory-bytes{offset} {} {count}", quote(address))
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

fn quote_required(value: Option<&str>, field: &'static str) -> Result<String, ApiError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(quote)
        .ok_or_else(|| ApiError::bad_request("missing_argument", format!("{field} is required")))
}

#[cfg(test)]
mod tests {
    use super::memory_read_command;

    #[test]
    fn memory_read_uses_mi_option_order_and_quotes_address_expressions() {
        assert_eq!(
            memory_read_command("$sp + 16", 128, Some(-8)),
            "-data-read-memory-bytes -o -8 \"$sp + 16\" 128"
        );
        assert_eq!(
            memory_read_command("0x1000", 32, None),
            "-data-read-memory-bytes \"0x1000\" 32"
        );
    }
}
