use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::state::StateMgr;

use super::{
    api, backtrace::DistributedBacktraceService, breakpoint::BreakpointService,
    execution::ExecutionService, input::ParsedInputCmd, query::QueryProjector, router::Target,
    CommandOutcome, Presentation,
};

/// Command operation selected by the engine after parsing and target resolution.
///
/// # Contract Semantics
///
/// Implementors of this trait are responsible for:
/// 1. **Preserving Target Semantics**: The target from `ParsedInputCmd` must be honored
///    unless the handler has specific routing requirements (e.g., `ThreadSelectHandler`
///    may adjust targets for thread selection commands)
///
/// 2. **Async Execution**: All command processing is async to support routing operations
///    that may involve network I/O to distributed debuggee processes.
///
/// 3. **Structured Completion**: Handlers return a semantic outcome. They never
///    print, format an error, or detach work; those are ingress responsibilities.
#[async_trait]
pub trait Handler: Send + Sync + std::fmt::Debug {
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome>;
}

#[derive(Debug)]
pub struct DefaultHandler;

impl DefaultHandler {
    pub fn new() -> Self {
        DefaultHandler
    }
}

#[async_trait]
impl Handler for DefaultHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = api::parsed(cmd)?.execute().await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }
}

#[derive(Debug)]
pub struct BreakInsertHandler {
    service: Arc<BreakpointService>,
}

impl BreakInsertHandler {
    pub(crate) fn new(service: Arc<BreakpointService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for BreakInsertHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.insert(cmd).await
    }
}

#[derive(Debug)]
pub struct BreakDeleteHandler {
    service: Arc<BreakpointService>,
}

impl BreakDeleteHandler {
    pub(crate) fn new(service: Arc<BreakpointService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for BreakDeleteHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.delete(cmd).await
    }
}

#[derive(Debug)]
pub struct ThreadInfoHandler {
    projector: QueryProjector,
}

impl ThreadInfoHandler {
    pub fn new(state: Arc<StateMgr>) -> Self {
        Self {
            projector: QueryProjector::new(state),
        }
    }
}

#[async_trait]
impl Handler for ThreadInfoHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        match cmd.target {
            Target::Thread(tid) => {
                let (_, local_tid) = self.projector.resolve_thread(tid)?;
                let thrd_info_cmd = format!(
                    "{}-thread-info {}",
                    cmd.external_token
                        .map(|token| { token.to_string() })
                        .unwrap_or("".to_string()),
                    local_tid
                );
                let response = api::command(&thrd_info_cmd)?
                    .target(Target::Thread(tid))
                    .execute()
                    .await?;
                let response = self.projector.project_threads(response)?;
                Ok(CommandOutcome::response(response, Presentation::ThreadInfo))
            }
            _ => {
                let response = api::parsed(cmd)?.execute().await?;
                let response = self.projector.project_threads(response)?;
                Ok(CommandOutcome::response(response, Presentation::ThreadInfo))
            }
        }
    }
}

#[derive(Debug)]
pub struct ContinueHandler {
    service: Arc<ExecutionService>,
}

impl ContinueHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for ContinueHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.continue_command(cmd).await
    }
}

#[derive(Debug)]
pub struct InterruptHandler {
    service: Arc<ExecutionService>,
}

impl InterruptHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for InterruptHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.interrupt(cmd).await
    }
}

#[derive(Debug)]
pub struct ListHandler;

impl ListHandler {
    pub fn new() -> Self {
        ListHandler
    }
}

#[async_trait]
impl Handler for ListHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        // FIXME: a naive implementation here, just select the first session
        // This command is need for CLI (to list out sources), but probably not for GUI?
        let response = api::parsed(cmd)?
            .target(Target::Session(1))
            .execute()
            .await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }
}

pub struct ThreadSelectHandler {
    state: Arc<StateMgr>,
}

impl std::fmt::Debug for ThreadSelectHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ThreadSelectHandler").finish()
    }
}

impl ThreadSelectHandler {
    pub fn new(state: Arc<StateMgr>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Handler for ThreadSelectHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let parts = cmd.args.split_whitespace().collect::<Vec<_>>();
        if !parts.is_empty() {
            let gtid = parts.last().unwrap().parse::<u64>()?;
            let (sid, tid) = self
                .state
                .local_thread_id(gtid)
                .ok_or_else(|| anyhow!("Unknown global thread {}", gtid))?
                .into();
            let target = Target::Session(sid);
            let response = api::command(&format!("-thread-select {}", tid))?
                .target(target)
                .execute()
                .await?;
            Ok(CommandOutcome::response(response, Presentation::Plain))
        } else {
            let response = api::parsed(cmd)?.execute().await?;
            Ok(CommandOutcome::response(response, Presentation::Plain))
        }
    }
}

#[derive(Debug)]
pub struct ListGroupsHandler {
    projector: QueryProjector,
}

impl ListGroupsHandler {
    pub fn new(state: Arc<StateMgr>) -> Self {
        Self {
            projector: QueryProjector::new(state),
        }
    }
}

#[async_trait]
impl Handler for ListGroupsHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = api::parsed(cmd)?
            .target(Target::Broadcast)
            .execute()
            .await?;
        let response = self.projector.project_processes(response)?;
        Ok(CommandOutcome::response(
            response,
            Presentation::ProcessReadable,
        ))
    }
}

#[derive(Debug)]
pub struct DistributeBacktraceHandler {
    service: Arc<DistributedBacktraceService>,
}

impl DistributeBacktraceHandler {
    pub(crate) fn new(service: Arc<DistributedBacktraceService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for DistributeBacktraceHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.execute(cmd).await
    }
}

#[derive(Debug)]
pub struct ExecNextHandler {
    service: Arc<ExecutionService>,
}

impl ExecNextHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for ExecNextHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.next(cmd).await
    }
}

#[derive(Debug)]
pub struct ExecFinishHandler {
    service: Arc<ExecutionService>,
}

impl ExecFinishHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for ExecFinishHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.finish(cmd).await
    }
}

#[derive(Debug)]
pub struct ExecStepHandler {
    service: Arc<ExecutionService>,
}

impl ExecStepHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for ExecStepHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.step(cmd).await
    }
}

#[derive(Debug)]
pub struct ExecJumpHandler {
    service: Arc<ExecutionService>,
}

impl ExecJumpHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for ExecJumpHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.jump(cmd).await
    }
}

#[derive(Debug)]
pub struct SendSignalHandler {
    service: Arc<ExecutionService>,
}

impl SendSignalHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for SendSignalHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.send_signal(cmd).await
    }
}

#[derive(Debug)]
pub struct ListSignalsHandler {
    service: Arc<ExecutionService>,
}

impl ListSignalsHandler {
    pub(crate) fn new(service: Arc<ExecutionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for ListSignalsHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.list_signals(cmd).await
    }
}
