use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use super::{
    api::CommandExecutor,
    backtrace::DistributedBacktraceService,
    breakpoint::{BreakpointEventPublisher, BreakpointService},
    dispatcher::CommandDispatcher,
    execution::ExecutionService,
    framework_adapter::FrameworkCommandAdapter,
    input::ParsedInputCmd,
    router::{Router, Target},
    transaction::TransactionCoordinator,
    CommandOutcome, Presentation,
};
use crate::{
    common::Config,
    debugger::DebuggerBackend,
    feature::{proclet_query::ProcletQueryService, proclet_restore::ProcletRestorationMgr},
    source::resolver::SourceResolver,
    state::GroupOperationCoordinator,
    state::RuntimeModel,
    state::StateMgr,
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
    dispatcher: CommandDispatcher,
    router: Arc<Router>,
    state: Arc<StateMgr>,
    source_resolver: Arc<SourceResolver>,
    model: Arc<RuntimeModel>,
    proclet_queries: Arc<ProcletQueryService>,
    detached_slots: Arc<Semaphore>,
}

impl CommandEngine {
    pub(crate) fn new(
        adapter: Arc<dyn FrameworkCommandAdapter>,
        router: Arc<Router>,
        breakpoint_events: Arc<BreakpointEventPublisher>,
        group_operations: Arc<GroupOperationCoordinator>,
        source_resolver: Arc<SourceResolver>,
        model: Arc<RuntimeModel>,
        config: Arc<Config>,
        backend: Arc<dyn DebuggerBackend>,
        proclet_restoration: Arc<ProcletRestorationMgr>,
        proclet_queries: Arc<ProcletQueryService>,
    ) -> Arc<Self> {
        let state = Arc::clone(model.state());
        let executor = CommandExecutor::new(Arc::clone(&router));
        let transactions = TransactionCoordinator::new(Arc::clone(&state), Arc::clone(&router));
        let breakpoint_service = Arc::new(BreakpointService::new(
            Arc::clone(model.breakpoints()),
            Arc::clone(model.groups()),
            breakpoint_events,
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
            executor.clone(),
            transactions,
            proclet_restoration,
        ));
        let dispatcher = CommandDispatcher::new(
            breakpoint_service,
            execution_service,
            backtrace_service,
            Arc::clone(&state),
            executor,
        );

        Arc::new(Self {
            dispatcher,
            router,
            state,
            source_resolver,
            model,
            proclet_queries,
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
                return Ok(self.execute_internal(internal).await);
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

    async fn execute_internal(&self, command: &str) -> CommandOutcome {
        match command {
            "p-session-meta" => info!("p-session-meta: {:?}", self.model.state().sessions()),
            "p-group-mgr" => info!("p-group-mgr: {:#?}", self.model.groups()),
            "p-bkpt-mgr" => info!("p-bkpt-mgr: {:#?}", self.model.breakpoints()),
            "p-proclet-mgr" => info!("p-proclet-mgr: {:#?}", self.model.proclets()),
            _ if command.starts_with("s-cmd ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                if parts.len() < 3 {
                    info!("Usage: s-cmd <session_id> <cmd>");
                    return CommandOutcome::empty();
                }
                let Ok(sid) = parts[1].parse::<u64>() else {
                    warn!("Invalid session id: {}", parts[1]);
                    return CommandOutcome::empty();
                };
                let raw = parts[2..].join(" ");
                match raw.try_into() {
                    Ok(parsed) => {
                        let parsed: ParsedInputCmd = parsed;
                        let (_, command) = parsed.to_command(self.router.next_internal_token());
                        match self.router.execute(Target::Session(sid), command).await {
                            Ok(response) => {
                                return CommandOutcome::response(response, Presentation::Plain);
                            }
                            Err(error) => warn!(?error, "failed to send internal command"),
                        }
                    }
                    Err(error) => warn!(?error, "failed to parse internal command"),
                }
            }
            _ if command.starts_with("q-proclet ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                if let Some(Ok(proclet_id)) = parts.get(1).map(|id| id.parse::<u64>()) {
                    match self.proclet_queries.query(proclet_id).await {
                        Ok(proclet) => info!("Proclet: {:?}", proclet),
                        Err(error) => warn!(?error, "failed to query proclet"),
                    }
                }
            }
            _ => {}
        }
        CommandOutcome::empty()
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
        self.dispatcher
            .dispatch(parsed)
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
