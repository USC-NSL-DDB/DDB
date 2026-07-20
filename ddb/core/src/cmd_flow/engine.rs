use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use tokio::sync::Semaphore;
use tracing::{debug, error, info};

use super::{
    api::CommandExecutor,
    backtrace::DistributedBacktraceService,
    breakpoint::BreakpointService,
    execution::ExecutionService,
    framework_adapter::FrameworkCommandAdapter,
    handler::{
        BreakDeleteHandler, BreakInsertHandler, ContinueHandler, DefaultHandler,
        DistributeBacktraceHandler, ExecFinishHandler, ExecJumpHandler, ExecNextHandler,
        ExecStepHandler, Handler, InterruptHandler, ListGroupsHandler, ListHandler,
        ListSignalsHandler, SendSignalHandler, ThreadInfoHandler, ThreadSelectHandler,
    },
    input::ParsedInputCmd,
    router::{Router, Target},
    transaction::TransactionCoordinator,
    CommandOutcome,
};
use crate::{
    common::Config, debugger::DebuggerBackend, feature::proclet_restore::ProcletRestorationMgr,
    group_operation::GroupOperationCoordinator, notification::NotificationManager,
    runtime_model::RuntimeModel, source::resolver::SourceResolver, state::StateMgr,
};

const DETACHED_COMMAND_LIMIT: usize = 256;

/// Failure returned at the user-command boundary, including the MI token when
/// parsing progressed far enough to preserve it.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct CommandError {
    external_token: Option<u64>,
    #[source]
    source: anyhow::Error,
}

impl CommandError {
    fn new(external_token: Option<u64>, source: anyhow::Error) -> Self {
        Self {
            external_token,
            source,
        }
    }

    pub fn external_token(&self) -> Option<u64> {
        self.external_token
    }
}

/// Single control-plane entry point for user-originated debugger commands.
///
/// The engine owns parsing policy, target defaults, operation selection, and
/// detached-work admission. Session runtimes continue to own transport I/O,
/// response correlation, and per-session ordering.
pub struct CommandEngine {
    handlers: HashMap<String, Arc<dyn Handler>>,
    default_handler: Arc<dyn Handler>,
    router: Arc<Router>,
    state: Arc<StateMgr>,
    source_resolver: Arc<SourceResolver>,
    detached_slots: Arc<Semaphore>,
}

impl CommandEngine {
    pub(crate) fn new(
        adapter: Arc<dyn FrameworkCommandAdapter>,
        router: Arc<Router>,
        notifications: Arc<NotificationManager>,
        group_operations: Arc<GroupOperationCoordinator>,
        source_resolver: Arc<SourceResolver>,
        model: Arc<RuntimeModel>,
        config: Arc<Config>,
        backend: Arc<dyn DebuggerBackend>,
        proclet_restoration: Arc<ProcletRestorationMgr>,
    ) -> Arc<Self> {
        let state = Arc::clone(model.state());
        let executor = CommandExecutor::new(Arc::clone(&router));
        let transactions = TransactionCoordinator::new(Arc::clone(&state), Arc::clone(&router));
        let breakpoint_service = Arc::new(BreakpointService::new(
            Arc::clone(model.breakpoints()),
            Arc::clone(model.groups()),
            notifications,
            executor.clone(),
            group_operations,
        ));
        let execution_service = Arc::new(ExecutionService::new(
            Arc::clone(&state),
            Arc::clone(&config),
            Arc::clone(&proclet_restoration),
            executor.clone(),
            transactions.clone(),
            backend,
        ));
        let backtrace_service = Arc::new(DistributedBacktraceService::new(
            adapter,
            Arc::clone(&state),
            config,
            executor,
            transactions,
            proclet_restoration,
        ));
        let mut handlers: HashMap<String, Arc<dyn Handler>> = HashMap::new();
        handlers.insert(
            "-break-insert".into(),
            Arc::new(BreakInsertHandler::new(Arc::clone(&breakpoint_service))),
        );
        handlers.insert(
            "-break-delete".into(),
            Arc::new(BreakDeleteHandler::new(breakpoint_service)),
        );
        handlers.insert(
            "-thread-info".into(),
            Arc::new(ThreadInfoHandler::new(Arc::clone(&state))),
        );
        handlers.insert(
            "-exec-continue".into(),
            Arc::new(ContinueHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-record-time-and-continue".into(),
            Arc::new(ContinueHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-exec-interrupt".into(),
            Arc::new(InterruptHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert("-file-list-lines".into(), Arc::new(ListHandler::new()));
        handlers.insert(
            "-thread-select".into(),
            Arc::new(ThreadSelectHandler::new(Arc::clone(&state))),
        );
        handlers.insert(
            "-bt-remote".into(),
            Arc::new(DistributeBacktraceHandler::new(backtrace_service)),
        );
        handlers.insert(
            "-list-thread-groups".into(),
            Arc::new(ListGroupsHandler::new(Arc::clone(&state))),
        );
        handlers.insert(
            "-exec-next".into(),
            Arc::new(ExecNextHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-exec-step".into(),
            Arc::new(ExecStepHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-exec-finish".into(),
            Arc::new(ExecFinishHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-record-time-and-next".into(),
            Arc::new(ExecNextHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-record-time-and-step".into(),
            Arc::new(ExecStepHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-record-time-and-finish".into(),
            Arc::new(ExecFinishHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-exec-jump".into(),
            Arc::new(ExecJumpHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-send-signal".into(),
            Arc::new(SendSignalHandler::new(Arc::clone(&execution_service))),
        );
        handlers.insert(
            "-list-signals".into(),
            Arc::new(ListSignalsHandler::new(Arc::clone(&execution_service))),
        );

        Arc::new(Self {
            handlers,
            default_handler: Arc::new(DefaultHandler::new()),
            router,
            state,
            source_resolver,
            detached_slots: Arc::new(Semaphore::new(DETACHED_COMMAND_LIMIT)),
        })
    }

    /// Execute one CLI command to semantic completion. CLI commands without an
    /// explicit target follow the currently selected thread, then broadcast.
    pub async fn execute_cli(&self, raw: &str) -> Result<CommandOutcome, CommandError> {
        if let Some(internal) = raw.trim().strip_prefix(':') {
            if internal == "p-source-resolver" {
                info!("p-source-resolver: {:#?}", self.source_resolver);
            } else if let Some(path) = internal.strip_prefix("p-resolve-src ") {
                self.source_resolver
                    .resolve_path(path)
                    .await
                    .map_err(|source| CommandError::new(None, source))?;
            } else {
                self.router.handle_internal_cmd(internal);
            }
            return Ok(CommandOutcome::empty());
        }

        let default_target = self
            .state
            .current_thread_id()
            .map(Target::Thread)
            .unwrap_or(Target::Broadcast);
        let parsed = Self::prepare(raw, None, default_target)
            .map_err(|source| CommandError::new(None, source))?;
        self.dispatch(parsed).await
    }

    /// Execute one API command to semantic completion. API commands without an
    /// embedded or explicit target broadcast, matching the documented API policy.
    pub async fn execute_api(
        &self,
        raw: &str,
        target: Option<Target>,
    ) -> Result<CommandOutcome, CommandError> {
        let parsed = Self::prepare(raw, target, Target::Broadcast)
            .map_err(|source| CommandError::new(None, source))?;
        self.dispatch(parsed).await
    }

    /// Admit bounded detached API work. Parsing and capacity admission happen
    /// before success is returned to the caller; execution uses the same path as
    /// a waiting API request.
    pub async fn submit_api(
        self: &Arc<Self>,
        raw: &str,
        target: Option<Target>,
    ) -> Result<(), CommandError> {
        let parsed = Self::prepare(raw, target, Target::Broadcast)
            .map_err(|source| CommandError::new(None, source))?;
        let external_token = parsed.external_token;
        let permit = Arc::clone(&self.detached_slots)
            .acquire_owned()
            .await
            .map_err(|error| CommandError::new(external_token, error.into()))?;
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = engine.dispatch(parsed).await {
                error!(?error, "detached command failed");
            }
        });
        Ok(())
    }

    async fn dispatch(&self, parsed: ParsedInputCmd) -> Result<CommandOutcome, CommandError> {
        let external_token = parsed.external_token;
        let prefix = parsed.prefix.clone();
        debug!(%prefix, target = ?parsed.target, "dispatching command");
        let handler = self.handlers.get(&prefix).unwrap_or(&self.default_handler);
        handler
            .process_cmd(parsed)
            .await
            .map_err(|source| CommandError::new(external_token, source))
    }

    fn prepare(
        raw: &str,
        explicit_target: Option<Target>,
        default_target: Target,
    ) -> Result<ParsedInputCmd> {
        let mut parsed: ParsedInputCmd = raw.try_into()?;
        if let Some(target) = explicit_target {
            parsed.target = target;
        } else if matches!(parsed.target, Target::Unspecified) {
            parsed.target = default_target;
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_target_wins_over_ingress_default() {
        let parsed =
            CommandEngine::prepare("17-thread-info --session 4", None, Target::Broadcast).unwrap();
        assert_eq!(parsed.external_token, Some(17));
        assert_eq!(parsed.target, Target::Session(4));
    }

    #[test]
    fn explicit_api_target_wins_over_embedded_target() {
        let parsed = CommandEngine::prepare(
            "-thread-info --session 4",
            Some(Target::Session(9)),
            Target::Broadcast,
        )
        .unwrap();
        assert_eq!(parsed.target, Target::Session(9));
    }

    #[test]
    fn ingress_default_only_resolves_unspecified_target() {
        let parsed = CommandEngine::prepare("-thread-info", None, Target::Session(6)).unwrap();
        assert_eq!(parsed.target, Target::Session(6));
    }
}
