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

/// Get the global notification manager
pub fn get_notif_mgr() -> std::sync::Arc<NotificationManager> {
    crate::context::app_context().notification_manager().clone()
}
