use std::sync::Arc;

use anyhow::Result;

use crate::{
    api::read_model::ApiQueries,
    cmd_flow::{
        api::CommandExecutor, breakpoint::BreakpointEventPublisher, engine::CommandEngine,
        event::DebuggerEventReducer, router::Router,
    },
    common::Config,
    dbg_mgr::DbgManager,
    debugger::DebuggerBackend,
    feature::{proclet_query::ProcletQueryService, proclet_restore::ProcletRestorationMgr},
    notification::NotificationManager,
    plugin::FrameworkPlugin,
    shutdown::ShutdownCtrl,
    source::{
        catalog::SourceCatalog,
        resolver::{SourceResolutionPolicy, SourceResolver},
    },
    state::GroupOperationCoordinator,
    state::RuntimeModel,
    status::RuntimeStatus,
};

/// Immutable service graph owned by one application runtime.
pub(crate) struct ApplicationServices {
    config: Arc<Config>,
    command_engine: Arc<CommandEngine>,
    notification_manager: Arc<NotificationManager>,
    api_queries: Arc<ApiQueries>,
    debugger_manager: Arc<DbgManager>,
}

impl ApplicationServices {
    async fn build(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
        shutdown: Arc<ShutdownCtrl>,
    ) -> Result<Arc<Self>> {
        let runtime_model = RuntimeModel::new();
        let notification_manager = Arc::new(NotificationManager::new());
        let breakpoint_events = BreakpointEventPublisher::new(Arc::clone(&notification_manager));
        let event_reducer =
            DebuggerEventReducer::new(Arc::clone(&runtime_model), Arc::clone(&breakpoint_events));
        let command_router = Arc::new(Router::new(Arc::clone(&runtime_model)));
        let command_executor = CommandExecutor::new(Arc::clone(&command_router));
        let proclet_queries =
            ProcletQueryService::connect(config.as_ref(), plugin.as_ref()).await?;
        let proclet_restoration = Arc::new(ProcletRestorationMgr::new(
            Arc::clone(runtime_model.proclets()),
            command_executor.clone(),
            Arc::clone(&proclet_queries),
        ));
        let group_operations = Arc::new(GroupOperationCoordinator::new());
        let source_resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::clone(runtime_model.groups()),
            command_executor,
            SourceResolutionPolicy::configured(),
        );
        let api_queries = ApiQueries::new(
            Arc::clone(&runtime_model),
            Arc::clone(&command_router),
            Arc::clone(&source_resolver),
        );
        let command_engine = CommandEngine::new(
            plugin.command_adapter(),
            Arc::clone(&command_router),
            Arc::clone(&breakpoint_events),
            Arc::clone(&group_operations),
            Arc::clone(&source_resolver),
            Arc::clone(&runtime_model),
            Arc::clone(&config),
            Arc::clone(&backend),
            proclet_restoration,
            proclet_queries,
        );
        let debugger_manager = DbgManager::new(
            Arc::clone(&config),
            Arc::clone(&plugin),
            Arc::clone(&backend),
            Arc::clone(&runtime_model),
            Arc::clone(&command_router),
            Arc::clone(&notification_manager),
            breakpoint_events,
            Arc::clone(&group_operations),
            Arc::clone(&source_resolver),
            Arc::clone(&event_reducer),
            shutdown,
        );

        Ok(Arc::new(Self {
            config,
            command_engine,
            notification_manager,
            api_queries,
            debugger_manager,
        }))
    }

    pub(crate) fn config(&self) -> &Arc<Config> {
        &self.config
    }

    pub(crate) fn command_engine(&self) -> &Arc<CommandEngine> {
        &self.command_engine
    }

    pub(crate) fn notification_manager(&self) -> &Arc<NotificationManager> {
        &self.notification_manager
    }

    pub(crate) fn api_queries(&self) -> &Arc<ApiQueries> {
        &self.api_queries
    }

    pub(crate) fn debugger_manager(&self) -> &Arc<DbgManager> {
        &self.debugger_manager
    }
}

/// Owns the service graph and lifecycle state for one DDB instance.
pub(crate) struct ApplicationRuntime {
    services: Arc<ApplicationServices>,
    shutdown: Arc<ShutdownCtrl>,
    status: Arc<RuntimeStatus>,
}

impl ApplicationRuntime {
    pub(crate) async fn new(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
    ) -> Result<Self> {
        let shutdown = Arc::new(ShutdownCtrl::new());
        let status = Arc::new(RuntimeStatus::new());
        let services =
            ApplicationServices::build(config, plugin, backend, Arc::clone(&shutdown)).await?;
        Ok(Self {
            services,
            shutdown,
            status,
        })
    }

    pub(crate) fn services(&self) -> &Arc<ApplicationServices> {
        &self.services
    }

    pub(crate) fn shutdown(&self) -> &Arc<ShutdownCtrl> {
        &self.shutdown
    }

    pub(crate) fn status(&self) -> &Arc<RuntimeStatus> {
        &self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_state_is_owned_per_runtime() {
        let first_shutdown = ShutdownCtrl::new();
        let second_shutdown = ShutdownCtrl::new();

        first_shutdown.trigger_once(crate::shutdown::ShutdownCause::UserExit);

        assert!(first_shutdown.should_shutdown());
        assert!(!second_shutdown.should_shutdown());
    }

    #[tokio::test]
    async fn complete_service_graphs_do_not_share_runtime_state() {
        let config = Arc::new(Config::default());
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        let backend = crate::debugger::resolve_debugger_backend(config.as_ref());

        let first = ApplicationRuntime::new(
            Arc::clone(&config),
            Arc::clone(&plugin),
            Arc::clone(&backend),
        )
        .await
        .unwrap();
        let second = ApplicationRuntime::new(config, plugin, backend)
            .await
            .unwrap();

        assert!(!Arc::ptr_eq(
            first.services().api_queries().model(),
            second.services().api_queries().model(),
        ));

        first
            .shutdown()
            .trigger_once(crate::shutdown::ShutdownCause::UserExit);
        assert!(first.shutdown().should_shutdown());
        assert!(!second.shutdown().should_shutdown());
    }
}
