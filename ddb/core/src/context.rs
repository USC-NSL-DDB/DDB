use std::sync::{Arc, OnceLock, Weak};

use crate::{
    cmd_flow::{engine::CommandEngine, router::Router},
    dbg_mgr::DbgManager,
    notification::NotificationManager,
    plugin::FrameworkCommandAdapter,
    shutdown::ShutdownCtrl,
    status::RuntimeStatus,
};

/// Owns the process-wide services and their dependency boundaries.
pub struct AppContext {
    command_engine: Arc<CommandEngine>,
    command_router: Arc<Router>,
    notification_manager: Arc<NotificationManager>,
    shutdown: ShutdownCtrl,
    runtime_status: RuntimeStatus,
    debugger_manager: OnceLock<Weak<DbgManager>>,
}

impl AppContext {
    fn new(command_adapter: Arc<dyn FrameworkCommandAdapter>) -> Self {
        let command_router = Arc::new(Router::new());
        let command_engine = CommandEngine::new(command_adapter, Arc::clone(&command_router));

        Self {
            command_engine,
            command_router,
            notification_manager: Arc::new(NotificationManager::new()),
            shutdown: ShutdownCtrl::new(),
            runtime_status: RuntimeStatus::new(),
            debugger_manager: OnceLock::new(),
        }
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

pub fn init_app_context(command_adapter: Arc<dyn FrameworkCommandAdapter>) -> &'static AppContext {
    APP_CONTEXT.get_or_init(|| AppContext::new(command_adapter))
}

pub fn app_context() -> &'static AppContext {
    #[cfg(test)]
    return APP_CONTEXT.get_or_init(|| AppContext::new(Arc::new(crate::plugin::GrpcAdapter)));

    #[cfg(not(test))]
    APP_CONTEXT.get().expect("AppContext is not initialized")
}
