use std::sync::Arc;

use anyhow::Result;

use crate::{
    api::read_model::ApiQueries,
    cmd_flow::{
        api::CommandExecutor,
        backtrace::DistributedBacktraceService,
        breakpoint::{BreakpointEventPublisher, BreakpointService},
        diagnostics::DiagnosticConsole,
        dispatcher::CommandDispatcher,
        engine::CommandEngine,
        event::DebuggerEventReducer,
        event_publisher::EventPublisher,
        execution::ExecutionService,
        query::{QueryProjector, QueryService},
        router::Router,
        transaction::TransactionCoordinator,
    },
    common::Config,
    dbg_mgr::DbgManager,
    debugger::DebuggerBackend,
    feature::{proclet_query::ProcletQueryService, proclet_restore::ProcletRestorationMgr},
    notification::NotificationManager,
    plugin::FrameworkPlugin,
    session::{factory::SessionFactory, supervisor::SessionSupervisor},
    shutdown::ShutdownCtrl,
    source::{
        catalog::SourceCatalog,
        resolver::{SourceResolutionPolicy, SourceResolver},
    },
    state::RuntimeModel,
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
    pub(super) async fn build(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
        shutdown: Arc<ShutdownCtrl>,
    ) -> Result<Arc<Self>> {
        let runtime_model = RuntimeModel::new();
        let notification_manager = Arc::new(NotificationManager::new());
        let (breakpoint_records, _breakpoint_record_sink) = EventPublisher::spawn();
        let breakpoint_events =
            BreakpointEventPublisher::new(Arc::clone(&notification_manager), breakpoint_records);
        let event_reducer =
            DebuggerEventReducer::new(Arc::clone(&runtime_model), Arc::clone(&breakpoint_events));
        let command_router = Arc::new(Router::new(Arc::clone(&runtime_model)));
        let command_executor = CommandExecutor::new(Arc::clone(&command_router));
        let proclet_queries =
            ProcletQueryService::connect(config.as_ref(), plugin.as_ref()).await?;
        let proclet_restoration = Arc::new(ProcletRestorationMgr::new(
            Arc::clone(&runtime_model),
            command_executor.clone(),
            Arc::clone(&proclet_queries),
        ));
        let source_resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::clone(&runtime_model) as _,
            command_executor.clone(),
            SourceResolutionPolicy::configured(),
        );
        let api_queries = ApiQueries::new(
            Arc::clone(&runtime_model),
            Arc::clone(&command_router),
            Arc::clone(&source_resolver),
        );
        let transactions =
            TransactionCoordinator::new(Arc::clone(&runtime_model), Arc::clone(&command_router));
        let breakpoint_service = Arc::new(BreakpointService::new(
            Arc::clone(&runtime_model),
            Arc::clone(&breakpoint_events),
            command_executor.clone(),
        ));
        let execution_service = Arc::new(ExecutionService::new(
            Arc::clone(&runtime_model),
            Arc::clone(&config),
            Arc::clone(&proclet_restoration),
            command_executor.clone(),
            transactions.clone(),
            Arc::clone(&backend),
        ));
        let backtrace_service = Arc::new(DistributedBacktraceService::new(
            plugin.command_adapter(),
            Arc::clone(&runtime_model),
            Arc::clone(&config),
            command_executor.clone(),
            transactions,
            proclet_restoration,
        ));
        let query_service = Arc::new(QueryService::new(
            command_executor.clone(),
            QueryProjector::new(Arc::clone(&runtime_model)),
        ));
        let dispatcher = CommandDispatcher::new(
            breakpoint_service,
            execution_service,
            backtrace_service,
            query_service,
            command_executor.clone(),
        );
        let diagnostics = DiagnosticConsole::new(
            Arc::clone(&runtime_model),
            Arc::clone(&source_resolver),
            proclet_queries,
            command_executor,
        );
        let command_engine =
            CommandEngine::new(dispatcher, diagnostics, Arc::clone(&runtime_model));
        let session_supervisor = SessionSupervisor::new(
            Arc::clone(&config),
            Arc::clone(&plugin),
            Arc::clone(&runtime_model),
            Arc::clone(&command_router),
            Arc::clone(&notification_manager),
            breakpoint_events,
            Arc::clone(&source_resolver),
            Arc::clone(&shutdown),
        );
        let session_factory = SessionFactory::new(
            Arc::clone(&config),
            Arc::clone(&backend),
            Arc::clone(&plugin),
            event_reducer,
        );
        let debugger_manager = DbgManager::new(
            Arc::clone(&config),
            plugin,
            session_supervisor,
            session_factory,
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
