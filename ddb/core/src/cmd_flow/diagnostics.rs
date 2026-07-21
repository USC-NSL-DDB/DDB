//! Operator diagnostic console: the `:`-prefixed command vocabulary.
//!
//! These commands exist for debugging DDB itself — dumping aggregate state,
//! probing source resolution, and sending raw commands to one session. They
//! execute through the same command executor as every other operation; the
//! console holds no routing or transport privileges of its own.

use std::{fmt, sync::Arc};

use anyhow::Result;
use tracing::{info, warn};

use super::{
    api::CommandExecutor, input::ParsedInputCmd, router::Target, CommandOutcome, Presentation,
};
use crate::{
    feature::proclet_query::ProcletQueryService, source::resolver::SourceResolver,
    state::RuntimeModel,
};

pub(crate) struct DiagnosticConsole {
    model: Arc<RuntimeModel>,
    source_resolver: Arc<SourceResolver>,
    proclet_queries: Arc<ProcletQueryService>,
    executor: CommandExecutor,
}

impl DiagnosticConsole {
    pub(crate) fn new(
        model: Arc<RuntimeModel>,
        source_resolver: Arc<SourceResolver>,
        proclet_queries: Arc<ProcletQueryService>,
        executor: CommandExecutor,
    ) -> Self {
        Self {
            model,
            source_resolver,
            proclet_queries,
            executor,
        }
    }

    pub(crate) async fn execute(&self, command: &str) -> Result<CommandOutcome> {
        match command {
            "p-source-resolver" => info!("p-source-resolver: {:#?}", self.source_resolver),
            "p-session-meta" => info!("p-session-meta: {:?}", self.model.state().sessions()),
            "p-group-mgr" => info!("p-group-mgr: {:#?}", self.model.groups()),
            "p-bkpt-mgr" => info!("p-bkpt-mgr: {:#?}", self.model.breakpoints()),
            "p-proclet-mgr" => info!("p-proclet-mgr: {:#?}", self.model.proclets()),
            _ if command.starts_with("p-resolve-src ") => {
                let path = &command["p-resolve-src ".len()..];
                self.source_resolver.resolve_path(path).await?;
            }
            _ if command.starts_with("s-cmd ") => return Ok(self.session_command(command).await),
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
        Ok(CommandOutcome::empty())
    }

    async fn session_command(&self, command: &str) -> CommandOutcome {
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
        match TryInto::<ParsedInputCmd>::try_into(raw) {
            Ok(parsed) => {
                let command = parsed.with_target(Target::Session(sid));
                match self.executor.execute_parsed(command).await {
                    Ok(response) => {
                        return CommandOutcome::response(response, Presentation::Plain);
                    }
                    Err(error) => warn!(?error, "failed to send internal command"),
                }
            }
            Err(error) => warn!(?error, "failed to parse internal command"),
        }
        CommandOutcome::empty()
    }
}

impl fmt::Debug for DiagnosticConsole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("DiagnosticConsole").finish()
    }
}
