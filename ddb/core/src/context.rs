use std::sync::{Arc, OnceLock, Weak};

use crate::{
    cmd_flow::{
        api::CommandExecutor, engine::CommandEngine, event::DebuggerEventReducer, router::Router,
    },
    common::Config,
    dbg_mgr::DbgManager,
    debugger::DebuggerBackend,
    feature::proclet_restore::ProcletRestorationMgr,
    group_operation::GroupOperationCoordinator,
    notification::NotificationManager,
    plugin::FrameworkPlugin,
    runtime_model::RuntimeModel,
    shutdown::ShutdownCtrl,
    source::{
        catalog::SourceCatalog,
        resolver::{SourceResolutionPolicy, SourceResolver},
    },
    status::RuntimeStatus,
};

/// Owns process-wide services and wires their dependency boundaries once.
pub struct AppContext {
    config: Arc<Config>,
    plugin: Arc<dyn FrameworkPlugin>,
    backend: Arc<dyn DebuggerBackend>,
    runtime_model: Arc<RuntimeModel>,
    event_reducer: Arc<DebuggerEventReducer>,
    command_engine: Arc<CommandEngine>,
    command_router: Arc<Router>,
    notification_manager: Arc<NotificationManager>,
    group_operations: Arc<GroupOperationCoordinator>,
    source_resolver: Arc<SourceResolver>,
    shutdown: ShutdownCtrl,
    runtime_status: RuntimeStatus,
    debugger_manager: OnceLock<Weak<DbgManager>>,
}

impl AppContext {
    fn new(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
    ) -> Self {
        let runtime_model = RuntimeModel::new();
        let notification_manager = Arc::new(NotificationManager::new());
        let event_reducer = DebuggerEventReducer::new(
            Arc::clone(&runtime_model),
            Arc::clone(&notification_manager),
        );
        let proclet_restoration = Arc::new(ProcletRestorationMgr::new(Arc::clone(
            runtime_model.proclets(),
        )));
        let command_router = Arc::new(Router::new(Arc::clone(&runtime_model)));
        let group_operations = Arc::new(GroupOperationCoordinator::new());
        let source_resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::clone(runtime_model.groups()),
            CommandExecutor::new(Arc::clone(&command_router)),
            SourceResolutionPolicy::configured(),
        );
        let command_engine = CommandEngine::new(
            plugin.command_adapter(),
            Arc::clone(&command_router),
            Arc::clone(&notification_manager),
            Arc::clone(&group_operations),
            Arc::clone(&source_resolver),
            Arc::clone(&runtime_model),
            Arc::clone(&config),
            Arc::clone(&backend),
            Arc::clone(&proclet_restoration),
        );

        Self {
            config,
            plugin,
            backend,
            runtime_model,
            event_reducer,
            command_engine,
            command_router,
            notification_manager,
            group_operations,
            source_resolver,
            shutdown: ShutdownCtrl::new(),
            runtime_status: RuntimeStatus::new(),
            debugger_manager: OnceLock::new(),
        }
    }

    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    pub fn plugin(&self) -> &Arc<dyn FrameworkPlugin> {
        &self.plugin
    }

    pub fn backend(&self) -> &Arc<dyn DebuggerBackend> {
        &self.backend
    }

    pub fn runtime_model(&self) -> &Arc<RuntimeModel> {
        &self.runtime_model
    }

    pub(crate) fn event_reducer(&self) -> &Arc<DebuggerEventReducer> {
        &self.event_reducer
    }

    pub fn command_engine(&self) -> &Arc<CommandEngine> {
        &self.command_engine
    }

    pub fn command_router(&self) -> &Arc<Router> {
        &self.command_router
    }

    pub fn notification_manager(&self) -> &Arc<NotificationManager> {
        &self.notification_manager
    }

    pub(crate) fn group_operations(&self) -> &Arc<GroupOperationCoordinator> {
        &self.group_operations
    }

    pub(crate) fn source_resolver(&self) -> &Arc<SourceResolver> {
        &self.source_resolver
    }

    pub fn shutdown(&self) -> &ShutdownCtrl {
        &self.shutdown
    }

    pub fn runtime_status(&self) -> &RuntimeStatus {
        &self.runtime_status
    }

    pub fn set_debugger_manager(&self, manager: Weak<DbgManager>) {
        self.debugger_manager
            .set(manager)
            .expect("DbgManager is already initialized");
    }

    pub fn debugger_manager(&self) -> Arc<DbgManager> {
        self.debugger_manager
            .get()
            .and_then(Weak::upgrade)
            .expect("DbgManager is not initialized")
    }
}

static APP_CONTEXT: OnceLock<AppContext> = OnceLock::new();

pub fn init_app_context(
    config: Arc<Config>,
    plugin: Arc<dyn FrameworkPlugin>,
    backend: Arc<dyn DebuggerBackend>,
) -> &'static AppContext {
    APP_CONTEXT.get_or_init(|| AppContext::new(config, plugin, backend))
}

pub fn app_context() -> &'static AppContext {
    #[cfg(test)]
    return APP_CONTEXT.get_or_init(|| {
        let config = Arc::new(Config::default());
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        let backend = crate::debugger::resolve_debugger_backend(config.as_ref());
        AppContext::new(config, plugin, backend)
    });

    #[cfg(not(test))]
    APP_CONTEXT.get().expect("AppContext is not initialized")
}
