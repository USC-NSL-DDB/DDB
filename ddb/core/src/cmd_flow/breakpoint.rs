use tracing::{debug, warn};

use crate::{
    dbg_parser::gdb_parser::{bkpt_deleted_payload, MIFormatter},
    notification::{get_notif_mgr, BreakpointChangeEvent, Notification, NotificationPayload},
    state::{BreakpointMgr, BreakpointStateChange},
};

/// Publishes the user-visible consequences of an automatic breakpoint state change.
///
/// The state manager deliberately has no output or notification dependencies. Lifecycle
/// and debugger-event callers invoke this publisher after releasing the repository lock.
pub(crate) async fn publish_breakpoint_state_change(
    breakpoints: &BreakpointMgr,
    change: BreakpointStateChange,
    context: &str,
) {
    let notification = match change {
        BreakpointStateChange::None => return,
        BreakpointStateChange::TargetChanged(breakpoint_id) => {
            let Some(breakpoint) = breakpoints.breakpoint(breakpoint_id) else {
                warn!(
                    breakpoint_id,
                    context, "breakpoint disappeared before its state change was published"
                );
                return;
            };
            let output =
                MIFormatter::format("=", "breakpoint-modified", Some(&breakpoint.into()), None);
            println!("{}", output);
            debug!("output: {}", output);
            BreakpointChangeEvent::TargetChanged(breakpoint_id)
        }
        BreakpointStateChange::Removed(breakpoint_id) => {
            let output = MIFormatter::format(
                "=",
                "breakpoint-deleted",
                Some(&bkpt_deleted_payload(breakpoint_id)),
                None,
            );
            println!("{}", output);
            debug!("output: {}", output);
            BreakpointChangeEvent::Removed(breakpoint_id)
        }
    };

    get_notif_mgr()
        .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
            notification,
        )))
        .await;
}

pub(crate) async fn publish_breakpoint_state_changes(
    breakpoints: &BreakpointMgr,
    changes: impl IntoIterator<Item = BreakpointStateChange>,
    context: &str,
) {
    for change in changes {
        publish_breakpoint_state_change(breakpoints, change, context).await;
    }
}
