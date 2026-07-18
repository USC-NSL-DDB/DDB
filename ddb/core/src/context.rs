use std::sync::{Arc, OnceLock, Weak};

use crate::{
    cmd_flow::{input::CmdHandler, router::Router, tracker::Tracker},
    dbg_mgr::DbgManager,
    notification::NotificationManager,
    plugin::FrameworkCommandAdapter,
    shutdown::ShutdownCtrl,
    status::RuntimeStatus,
};

/// Owns the mutable services that make up one DDB process.
///
/// Compatibility accessors in the individual modules currently delegate to
/// this context. Keeping construction and ownership here gives subsequent
/// refactors a single dependency boundary without forcing every caller to
/// change at once.
pub struct AppContext {
    command_handler: Arc<CmdHandler>,
    command_router: Arc<Router>,
    command_tracker: Arc<Tracker>,
    notification_manager: Arc<NotificationManager>,
    shutdown: ShutdownCtrl,
    runtime_status: RuntimeStatus,
    debugger_manager: OnceLock<Weak<DbgManager>>,
}

impl AppContext {
    fn new(command_adapter: Arc<dyn FrameworkCommandAdapter>) -> Self {
        let command_tracker = Tracker::new();
        let command_router = Arc::new(Router::new(Arc::clone(&command_tracker)));

        Self {
            command_handler: CmdHandler::new(command_adapter),
            command_router,
            command_tracker,
            notification_manager: Arc::new(NotificationManager::new()),
            shutdown: ShutdownCtrl::new(),
            runtime_status: RuntimeStatus::new(),
            debugger_manager: OnceLock::new(),
        }
    }

    pub fn command_handler(&self) -> &Arc<CmdHandler> {
        &self.command_handler
    }

    pub fn command_router(&self) -> &Arc<Router> {
        &self.command_router
    }

    pub fn command_tracker(&self) -> &Arc<Tracker> {
        &self.command_tracker
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
