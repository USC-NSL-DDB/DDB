use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::state::BreakpointSnapshot;

/// Notification envelope - versioned for extensibility
#[derive(Serialize, Clone, Debug)]
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
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", content = "data")]
pub enum NotificationPayload {
    /// Backend-neutral debugger records after DDB has projected local ids into
    /// global ids. API clients consume this instead of scraping MI stdout.
    DebuggerOutput(DebuggerOutputEvent),
    BreakpointChanged(BreakpointChangeEvent),
    SessionStatusChanged(SessionStatusEvent),
    SessionListChanged,
    Custom(CustomEvent),
}

#[derive(Serialize, Clone, Debug)]
pub struct DebuggerOutputEvent {
    pub records: Vec<DebuggerOutputRecord>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DebuggerOutputRecord {
    pub stream: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", content = "data")]
pub enum BreakpointChangeEvent {
    // Target group or session might changed.
    // The u64 is the bkpt id.
    // Consumer may need to refresh groups or sessions associated with the breakpoint.
    // Possible cases:
    // - target group's session list has changed.
    TargetChanged(u64),
    // Breakpoint removed.
    // The u64 is the bkpt id.
    // Consumer may need to remove the breakpoint from its list.
    Removed(u64),
    // Breakpoint added.
    Added(BreakpointSnapshot),
    // Breakpoint updated.
    // Possible cases:
    // - condition changed
    // - hit count changed
    // - subbkpt changed (added/removed/updated)
    Updated(BreakpointSnapshot),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn notification_new_populates_version_timestamp_and_unique_id() {
        let first = Notification::new(NotificationPayload::SessionListChanged);
        let second = Notification::new(NotificationPayload::SessionListChanged);

        assert_eq!(first.version, 1);
        assert!(first.timestamp > 0);
        assert_ne!(first.notification_id, second.notification_id);
    }

    #[test]
    fn custom_notification_payload_serializes_with_type_and_data() {
        let notification = Notification::new(NotificationPayload::Custom(CustomEvent {
            event_type: "reload".to_string(),
            data: json!({"ok": true}),
        }));

        let value = serde_json::to_value(notification).expect("notification should serialize");

        assert_eq!(value["payload"]["type"], "Custom");
        assert_eq!(value["payload"]["data"]["event_type"], "reload");
        assert_eq!(value["payload"]["data"]["data"]["ok"], true);
    }
}
