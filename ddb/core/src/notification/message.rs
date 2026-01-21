use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Notification envelope - versioned for extensibility
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Notification {
    pub version: u32,
    pub timestamp: i64,
    pub notification_id: Uuid,
    pub payload: NotificationPayload,
}

impl Notification {
    pub fn new(payload: NotificationPayload) -> Self {
        Self {
            version: 1,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            notification_id: Uuid::new_v4(),
            payload,
        }
    }
}

/// Extensible notification types using tagged enum
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "data")]
pub enum NotificationPayload {
    BreakpointChanged(BreakpointChangeEvent),
    SessionStatusChanged(SessionStatusEvent),
    Custom(CustomEvent),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BreakpointChangeEvent {
    pub session_id: u64,
    pub action: String,
    pub file: String,
    pub line: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionStatusEvent {
    pub session_id: u64,
    pub status: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CustomEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}
