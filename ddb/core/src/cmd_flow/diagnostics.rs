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
    api::CommandExecutor,
    engine::retire_legacy_stop_automation,
    input::{CommandMetadata, ParsedInputCmd},
    router::Target,
    CommandOutcome, Presentation,
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

    pub(crate) async fn execute(
        &self,
        command: &str,
        metadata: CommandMetadata,
    ) -> Result<CommandOutcome> {
        match command {
            "p-source-resolver" => info!("p-source-resolver: {:#?}", self.source_resolver),
            "p-session-meta" => {
                info!("p-session-meta: {:?}", self.model.session_snapshots().await)
            }
            "p-group-mgr" => info!("p-group-mgr: {:#?}", self.model.groups()),
            "p-bkpt-mgr" => {
                info!("p-bkpt-mgr: {:#?}", self.model.breakpoint_snapshots())
            }
            "p-proclet-mgr" => info!("p-proclet-mgr: {:#?}", self.model.proclet_owners()),
            _ if command.starts_with("p-resolve-src ") => {
                let path = &command["p-resolve-src ".len()..];
                self.source_resolver.resolve_path(path).await?;
            }
            _ if command.starts_with("s-cmd ") => {
                return Ok(self.session_command(command, metadata).await)
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
        Ok(CommandOutcome::empty())
    }

    async fn session_command(&self, command: &str, metadata: CommandMetadata) -> CommandOutcome {
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
                let command = parsed
                    .with_target(Target::Session(sid))
                    .with_metadata(metadata);
                if let Err(error) = retire_legacy_stop_automation(&self.model, &command, |target| {
                    self.executor.resolve_session_ids(target)
                }) {
                    warn!(?error, "failed to resolve internal command cancellation");
                    return CommandOutcome::empty();
                }
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

#[cfg(test)]
mod tests {
    use ddb_api_types::v2::OperationKind;

    use crate::{
        cmd_flow::router::Router,
        common::Config,
        plugin::resolve_framework_plugin,
        source::{
            catalog::SourceCatalog,
            resolver::{SourceResolutionPolicy, SourceResolver},
        },
    };

    use super::*;

    async fn console(model: &Arc<RuntimeModel>) -> DiagnosticConsole {
        let executor = CommandExecutor::new(Arc::new(Router::new(model.clone())));
        let source = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            model.clone(),
            executor.clone(),
            SourceResolutionPolicy::OnDemand,
        );
        let config = Config::from_str("Framework: unspecified\n").unwrap();
        let plugin = resolve_framework_plugin(&config);
        let proclets = ProcletQueryService::connect(&config, plugin.as_ref())
            .await
            .unwrap();
        DiagnosticConsole::new(model.clone(), source, proclets, executor)
    }

    #[tokio::test]
    async fn unrouted_diagnostic_session_resume_preserves_pending_automation() {
        let model = RuntimeModel::new();
        model.register_session(1, "diagnostic-target", None).await;
        model.complete_session_activation(1, None).await;
        let console = console(&model).await;
        console
            .execute("s-cmd 1 -exec-continue", CommandMetadata::default())
            .await
            .unwrap();
        // A model identity without a runtime route does not resolve an accepted
        // legacy target and must not retire automatic work.
        assert_eq!(model.execution_control_epoch().current(), 0);
    }

    #[tokio::test]
    async fn nonexistent_diagnostic_resume_does_not_retire_automation() {
        let model = RuntimeModel::new();
        let console = console(&model).await;
        console
            .execute("s-cmd 1 -exec-continue", CommandMetadata::default())
            .await
            .unwrap();
        assert_eq!(model.execution_control_epoch().current(), 0);
    }

    #[tokio::test]
    async fn admitted_diagnostic_resume_preserves_automation_from_after_acceptance() {
        let model = RuntimeModel::new();
        let console = console(&model).await;
        model.retire_stop_automation();
        let accepted_epoch = model.execution_control_epoch().current();
        console
            .execute(
                "s-cmd 1 -exec-continue",
                CommandMetadata {
                    operation_id: Some("accepted-raw-diagnostic".into()),
                    operation_kind: Some(OperationKind::RawCommand as u32),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(model.execution_control_epoch().current(), accepted_epoch);
    }

    #[tokio::test]
    async fn diagnostic_inspection_preserves_pending_automation() {
        let model = RuntimeModel::new();
        let console = console(&model).await;
        console
            .execute("s-cmd 1 -stack-list-frames", CommandMetadata::default())
            .await
            .unwrap();
        assert_eq!(model.execution_control_epoch().current(), 0);
    }
}
