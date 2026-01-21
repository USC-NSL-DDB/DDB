mod handler;
mod manager;
mod message;
mod subscriber;

pub use handler::{
    notification_status_handler, notification_subscribe_handler, test_notification_handler,
};
pub use manager::NotificationManager;
pub use message::{
    BreakpointChangeEvent, CustomEvent, Notification, NotificationPayload, SessionStatusEvent,
};

use std::sync::{Arc, OnceLock};

static NOTIFICATION_MGR: OnceLock<Arc<NotificationManager>> = OnceLock::new();

/// Initialize the global notification manager
pub fn init_notification_mgr() {
    NOTIFICATION_MGR.get_or_init(|| Arc::new(NotificationManager::new()));
}

/// Get the global notification manager
pub fn get_notif_mgr() -> Arc<NotificationManager> {
    NOTIFICATION_MGR
        .get()
        .expect("NotificationManager not initialized")
        .clone()
}
