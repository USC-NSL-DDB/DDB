use std::{fmt, sync::Arc};

use anyhow::{anyhow, bail, Result};
use futures::future::join_all;
use tracing::debug;

use crate::{
    common::Config,
    debugger::DebuggerBackend,
    feature::proclet_restore::ProcletRestorationMgr,
    state::{LocalThreadId, RuntimeModel, ThreadContext, ThreadStatus},
};

use super::{
    api::CommandExecutor,
    decoder::Payload,
    input::ParsedInputCmd,
    router::{
        CommandFanoutError, CommandFanoutReport, SessionCommandFailure, SessionCommandFailureKind,
        Target,
    },
    transaction::TransactionCoordinator,
    CommandOutcome, FinishedCmd, ParsedSessionResponse, Presentation,
};

pub(crate) struct ExecutionService {
    model: Arc<RuntimeModel>,
    config: Arc<Config>,
    proclet_restoration: Arc<ProcletRestorationMgr>,
    executor: CommandExecutor,
    transactions: TransactionCoordinator,
    backend: Arc<dyn DebuggerBackend>,
}

impl ExecutionService {
    pub(crate) fn new(
        model: Arc<RuntimeModel>,
        config: Arc<Config>,
        proclet_restoration: Arc<ProcletRestorationMgr>,
        executor: CommandExecutor,
        transactions: TransactionCoordinator,
        backend: Arc<dyn DebuggerBackend>,
    ) -> Self {
        Self {
            model,
            config,
            proclet_restoration,
            executor,
            transactions,
            backend,
        }
    }

    pub(crate) async fn continue_command(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        if self.config.conf.support_migration {
            self.proclet_restoration.reset().await;
        }

        let external_token = command.external_token;
        let sessions = self.executor.resolve_session_ids(&command.target)?;
        let continuations = sessions.into_iter().map(|session_id| {
            let command = command.clone();
            async move { (session_id, self.continue_session(command, session_id).await) }
        });
        let mut responses = Vec::<ParsedSessionResponse>::new();
        let mut failures = Vec::<SessionCommandFailure>::new();
        for (session_id, result) in join_all(continuations).await {
            match result {
                Ok(response) => responses.extend(response.get_responses().iter().cloned()),
                Err(error) => {
                    if let Some(fanout) = error.downcast_ref::<CommandFanoutError>() {
                        responses
                            .extend(fanout.report().completion().get_responses().iter().cloned());
                        failures.extend_from_slice(fanout.report().failures());
                    } else {
                        failures.push(SessionCommandFailure::new(
                            session_id,
                            SessionCommandFailureKind::ExecutionFailed,
                        ));
                    }
                }
            }
        }
        let response =
            CommandFanoutReport::new(external_token, responses, failures).into_result()?;
        Ok(CommandOutcome::response(response, Presentation::Unit))
    }

    pub(crate) async fn interrupt(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let command = command.with_prefix("-exec-interrupt-if-running");
        let response = self.executor.execute_parsed(command).await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }

    pub(crate) async fn next(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        self.thread_command(command, "exec-next").await
    }

    pub(crate) async fn step(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        self.thread_command(command, "exec-step").await
    }

    pub(crate) async fn finish(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        self.thread_command(command, "exec-finish").await
    }

    pub(crate) async fn jump(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        if !matches!(command.target, Target::Thread(_) | Target::Session(_)) {
            bail!("exec-jump command should specify a thread or session");
        }
        let response = self.executor.execute_parsed(command).await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }

    pub(crate) async fn send_signal(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let signal = command.args.trim();
        if signal.is_empty() {
            bail!("-send-signal command requires a signal argument");
        }
        let session_id = match command.target {
            Target::Session(session_id) => session_id,
            Target::Thread(global_thread_id) => {
                let LocalThreadId(session_id, _) = self
                    .model
                    .local_thread_id(global_thread_id)
                    .ok_or_else(|| anyhow!("Unknown global thread {}", global_thread_id))?;
                session_id
            }
            _ => bail!("-send-signal command should specify a thread or session"),
        };

        self.executor
            .execute(
                &self.backend.interrupt_command(),
                Target::Session(session_id),
            )
            .await?;

        let signal_command = self
            .backend
            .console_exec_command(&format!("signal {}", signal));
        let mut response = self
            .executor
            .execute(&signal_command, Target::Session(session_id))
            .await?;
        if let Some(token) = command.external_token {
            response.set_external_token(token);
        }
        debug!("-send-signal command completed: {}", signal_command);
        Ok(CommandOutcome::silent(response))
    }

    pub(crate) async fn list_signals(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        if !command.args.trim().is_empty() {
            bail!(
                "-list-signals command needs no argument. raw: {}",
                command.args
            );
        }
        let Target::Session(session_id) = command.target else {
            bail!("-list-signals command should specify a session");
        };

        let debugger_command = format!(
            "{}-list-signals",
            command
                .external_token
                .map(|token| token.to_string())
                .unwrap_or_default()
        );
        let response = self
            .executor
            .execute(&debugger_command, Target::Session(session_id))
            .await?;
        debug!("-list-signals command completed: {}", debugger_command);
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }

    async fn continue_session(
        &self,
        command: ParsedInputCmd,
        session_id: u64,
    ) -> Result<FinishedCmd> {
        let transaction = self
            .transactions
            .begin(session_id)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let snapshot = transaction
            .session_snapshot()
            .await
            .ok_or_else(|| anyhow!("Session {} disappeared during transaction", session_id))?;
        let in_custom_context = snapshot.in_custom_context;
        let current_context = snapshot.current_context;

        if in_custom_context {
            let context = current_context
                .ok_or_else(|| anyhow!("Session {} has no context to restore", session_id))?;
            let restore = self
                .executor
                .execute_exclusive(
                    &format!(
                        "-switch-context-custom {}",
                        prepare_context_switch_args(&context)
                    ),
                    Target::Thread(context.tid),
                    transaction.lease(),
                )
                .await?;
            let restored = restore.get_responses().len() == 1
                && Payload::first(&restore)?.string("message")? == "success";
            transaction.finish_context_restore(restored).await;
            if !restored {
                bail!("Failed to restore context for session {}", session_id);
            }
        }

        let execution_target = match command.target {
            Target::Thread(thread_id) => Target::Thread(thread_id),
            _ => Target::Session(session_id),
        };
        let response = self
            .executor
            .execute_parsed_exclusive(command, execution_target, transaction.lease())
            .await?;
        if response
            .get_responses()
            .iter()
            .all(|response| response.get_message() != "error")
        {
            transaction.mark_all_threads(ThreadStatus::RUNNING).await?;
        }
        Ok(response)
    }

    async fn thread_command(
        &self,
        command: ParsedInputCmd,
        operation: &'static str,
    ) -> Result<CommandOutcome> {
        require_thread_target(&command.target, operation)?;
        let response = self.executor.execute_parsed(command).await?;
        Ok(CommandOutcome::silent(response))
    }
}

impl fmt::Debug for ExecutionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ExecutionService").finish()
    }
}

fn prepare_context_switch_args(registers: &ThreadContext) -> String {
    registers
        .ctx
        .iter()
        .map(|(register, value)| format!("{}={}", register, value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn require_thread_target(target: &Target, operation: &str) -> Result<()> {
    if matches!(target, Target::Thread(_)) {
        Ok(())
    } else {
        bail!(
            "{} command should specify a thread id by --thread <gtid>",
            operation
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn context_switch_arguments_are_space_separated_register_assignments() {
        let context = ThreadContext {
            ctx: HashMap::from([("pc".to_string(), 4096), ("sp".to_string(), 8192)]),
            tid: crate::state::GlobalThreadId::new(7),
        };

        let args = prepare_context_switch_args(&context);

        assert!(args.contains("pc=4096"));
        assert!(args.contains("sp=8192"));
        assert_eq!(args.split_whitespace().count(), 2);
    }

    #[test]
    fn stepping_operations_require_a_thread_target() {
        assert!(require_thread_target(
            &Target::Thread(crate::state::GlobalThreadId::new(9)),
            "exec-step"
        )
        .is_ok());
        assert!(require_thread_target(&Target::Session(2), "exec-step").is_err());
    }
}
