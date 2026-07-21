use std::{collections::HashSet, fmt, sync::Arc};

use anyhow::{anyhow, bail, Context, Result};
use tracing::{debug, error, warn};

use crate::{
    debugger::gdb::parser::MIFormatter,
    notification::{BreakpointChangeEvent, Notification, NotificationManager, NotificationPayload},
    state::GroupOperationCoordinator,
    state::{
        BkptLoc, BreakpointMgr, BreakpointSnapshot, BreakpointStateChange, GroupId, GroupMgr,
        GroupSubBkpt, SessionSubBkpt, SubBkptType,
    },
};

use super::{
    api::CommandExecutor,
    breakpoint_mi::{bkpt_deleted_payload, bkpt_payload},
    decoder::BreakpointCreated,
    input::ParsedInputCmd,
    router::Target,
    CommandOutcome, Presentation,
};

/// Sole boundary for breakpoint notifications and automatic MI records.
pub(crate) struct BreakpointEventPublisher {
    notifications: Arc<NotificationManager>,
}

impl BreakpointEventPublisher {
    pub(crate) fn new(notifications: Arc<NotificationManager>) -> Arc<Self> {
        Arc::new(Self { notifications })
    }

    pub(crate) async fn publish_state_change(&self, change: BreakpointStateChange) {
        let event = match change {
            BreakpointStateChange::None => return,
            BreakpointStateChange::TargetChanged(breakpoint) => {
                let snapshot = BreakpointSnapshot::from(&breakpoint);
                let output = MIFormatter::format(
                    "=",
                    "breakpoint-modified",
                    Some(&bkpt_payload(&snapshot)),
                    None,
                );
                println!("{}", output);
                debug!("output: {}", output);
                BreakpointChangeEvent::TargetChanged(snapshot.id)
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
        self.broadcast(event).await;
    }

    pub(crate) async fn publish_state_changes(
        &self,
        changes: impl IntoIterator<Item = BreakpointStateChange>,
    ) {
        for change in changes {
            self.publish_state_change(change).await;
        }
    }

    pub(crate) async fn broadcast(&self, event: BreakpointChangeEvent) {
        self.notifications
            .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                event,
            )))
            .await;
    }
}

pub(crate) struct BreakpointService {
    breakpoints: Arc<BreakpointMgr>,
    groups: Arc<GroupMgr>,
    events: Arc<BreakpointEventPublisher>,
    executor: CommandExecutor,
    group_operations: Arc<GroupOperationCoordinator>,
}

impl BreakpointService {
    pub(crate) fn new(
        breakpoints: Arc<BreakpointMgr>,
        groups: Arc<GroupMgr>,
        events: Arc<BreakpointEventPublisher>,
        executor: CommandExecutor,
        group_operations: Arc<GroupOperationCoordinator>,
    ) -> Self {
        Self {
            breakpoints,
            groups,
            events,
            executor,
            group_operations,
        }
    }

    fn group_ids_for_breakpoint(&self, breakpoint_id: u64) -> Vec<GroupId> {
        self.breakpoints
            .breakpoint(breakpoint_id)
            .map(|breakpoint| {
                breakpoint
                    .sub_breakpoints()
                    .iter()
                    .filter_map(|sub_breakpoint| match sub_breakpoint.kind() {
                        SubBkptType::Group(group_breakpoint) => {
                            Some(group_breakpoint.target_group())
                        }
                        SubBkptType::Session(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) async fn insert(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        if !matches!(
            &command.target,
            Target::Session(_) | Target::Group(_) | Target::Multiple(_)
        ) {
            bail!("break-insert requires a session, group, or multiple target");
        }

        let debugger_command = command.full_cmd();
        let location = parse_breakpoint_location(&command.args)?;
        let breakpoint_id = self.breakpoints.add_breakpoint(location);

        match &command.target {
            Target::Session(session_id) => {
                if let Err(error) = self
                    .insert_for_session(breakpoint_id, &debugger_command, *session_id)
                    .await
                {
                    self.breakpoints.remove_breakpoint(breakpoint_id);
                    bail!(
                        "Failed to insert breakpoint into session {}: {}",
                        session_id,
                        error
                    );
                }
            }
            Target::Group(group_id) => {
                if let Err(error) = self
                    .insert_for_group(breakpoint_id, &debugger_command, *group_id)
                    .await
                {
                    self.breakpoints.remove_breakpoint(breakpoint_id);
                    bail!(
                        "Failed to insert breakpoint into group {}: {}",
                        group_id,
                        error
                    );
                }
            }
            Target::Multiple(targets) => {
                for target in deduplicate_insertion_targets(self.groups.as_ref(), targets) {
                    let result = match target {
                        Target::Session(session_id) => {
                            self.insert_for_session(breakpoint_id, &debugger_command, session_id)
                                .await
                        }
                        Target::Group(group_id) => {
                            self.insert_for_group(breakpoint_id, &debugger_command, group_id)
                                .await
                        }
                        _ => unreachable!("target planning only returns session and group targets"),
                    };
                    if let Err(error) = result {
                        warn!(
                            ?target,
                            %error,
                            "failed to insert breakpoint into one target"
                        );
                    }
                }
            }
            _ => unreachable!("target was validated before creating breakpoint state"),
        }

        if self.breakpoints.breakpoint_is_empty(breakpoint_id) == Some(true) {
            self.breakpoints.remove_breakpoint(breakpoint_id);
            bail!("Failed to insert breakpoint into any target.");
        }

        let breakpoint = self.breakpoints.breakpoint(breakpoint_id).ok_or_else(|| {
            anyhow!(
                "Failed to find inserted breakpoint with id {}",
                breakpoint_id
            )
        })?;
        let snapshot = BreakpointSnapshot::from(&breakpoint);
        let payload = bkpt_payload(&snapshot);
        self.events
            .broadcast(BreakpointChangeEvent::Added(snapshot))
            .await;

        Ok(CommandOutcome::completed(
            command.external_token,
            0,
            "done",
            Some(payload),
            Presentation::Plain,
        ))
    }

    pub(crate) async fn delete(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let args = command.args.trim();
        if args.is_empty() {
            bail!("No breakpoint id provided for deletion.");
        }

        if let Some((breakpoint_id, sub_breakpoint_id)) = args.split_once(char::is_whitespace) {
            let breakpoint_id = breakpoint_id
                .parse::<u64>()
                .with_context(|| format!("Invalid breakpoint id {}", breakpoint_id))?;
            let sub_breakpoint_id = sub_breakpoint_id
                .parse::<u64>()
                .with_context(|| format!("Invalid sub-breakpoint id {}", sub_breakpoint_id))?;
            let change = self
                .delete_sub_breakpoint(breakpoint_id, sub_breakpoint_id)
                .await?;
            return self
                .explicit_delete_outcome(command.external_token, change)
                .await;
        }

        let breakpoint_id = args
            .parse::<u64>()
            .with_context(|| format!("Invalid breakpoint id {}", args))?;
        let group_operations = self
            .group_operations
            .lock_many(self.group_ids_for_breakpoint(breakpoint_id))
            .await;
        for (session_id, local_breakpoint_id) in
            self.breakpoints.local_breakpoint_ids(breakpoint_id)
        {
            if let Err(error) = self
                .delete_local_breakpoint(session_id, local_breakpoint_id)
                .await
            {
                warn!(
                    session_id,
                    local_breakpoint_id,
                    %error,
                    "failed to delete local breakpoint"
                );
            }
        }

        self.breakpoints.remove_breakpoint(breakpoint_id);
        drop(group_operations);
        let record = MIFormatter::format(
            "=",
            "breakpoint-deleted",
            Some(&bkpt_deleted_payload(breakpoint_id)),
            None,
        );
        self.events
            .broadcast(BreakpointChangeEvent::Removed(breakpoint_id))
            .await;

        let mut outcome =
            CommandOutcome::completed(command.external_token, 0, "done", None, Presentation::Plain);
        outcome.insert_record(0, record);
        Ok(outcome)
    }

    async fn insert_for_group(
        &self,
        breakpoint_id: u64,
        debugger_command: &str,
        group_id: GroupId,
    ) -> Result<()> {
        let _group_operation = self.group_operations.lock(group_id).await;
        let group = self
            .groups
            .group_by_id(group_id)
            .ok_or_else(|| anyhow!("Group {} does not exist", group_id))?;
        let has_active_sessions = !group.session_ids().is_empty();
        let mut group_breakpoint = GroupSubBkpt::new(group_id);

        if has_active_sessions {
            let completion = self
                .executor
                .execute(debugger_command, Target::Group(group_id))
                .await?;
            for response in completion.get_responses() {
                let breakpoint = BreakpointCreated::decode(response)?;
                let _ = breakpoint.times;
                group_breakpoint.add_local_bkpt(response.get_sid(), breakpoint.local_id);
            }
        }

        self.breakpoints
            .add_sub_breakpoint(breakpoint_id, SubBkptType::Group(group_breakpoint));
        Ok(())
    }

    async fn insert_for_session(
        &self,
        breakpoint_id: u64,
        debugger_command: &str,
        session_id: u64,
    ) -> Result<()> {
        let completion = self
            .executor
            .execute(debugger_command, Target::Session(session_id))
            .await?;
        let breakpoint = BreakpointCreated::decode_first(&completion)?;
        let _ = breakpoint.times;
        self.breakpoints.add_sub_breakpoint(
            breakpoint_id,
            SubBkptType::Session(SessionSubBkpt::new(breakpoint.local_id, session_id)),
        );
        Ok(())
    }

    async fn delete_local_breakpoint(
        &self,
        session_id: u64,
        local_breakpoint_id: u64,
    ) -> Result<BreakpointStateChange> {
        let completion = self
            .executor
            .execute(
                &format!("-break-delete {}", local_breakpoint_id),
                Target::Session(session_id),
            )
            .await?;
        let response = completion.get_responses().first().ok_or_else(|| {
            anyhow!(
                "Debugger returned no response while deleting breakpoint {} from session {}",
                local_breakpoint_id,
                session_id
            )
        })?;
        if response.get_message() != "done" {
            bail!(
                "Failed to delete local breakpoint {} from session {}: debugger returned {}",
                local_breakpoint_id,
                session_id,
                response.get_message()
            );
        }

        Ok(self
            .breakpoints
            .record_local_bkpt_deletion(session_id, local_breakpoint_id))
    }

    async fn delete_sub_breakpoint(
        &self,
        breakpoint_id: u64,
        sub_breakpoint_id: u64,
    ) -> Result<BreakpointStateChange> {
        let mut sub_breakpoint = self
            .breakpoints
            .sub_breakpoint(breakpoint_id, sub_breakpoint_id)
            .ok_or_else(|| {
                anyhow!(
                    "No sub-breakpoint found for deletion with bkpt_id {} and subbkpt_id {}",
                    breakpoint_id,
                    sub_breakpoint_id
                )
            })?;
        let group_operation = match sub_breakpoint.kind() {
            SubBkptType::Group(group_breakpoint) => Some(
                self.group_operations
                    .lock(group_breakpoint.target_group())
                    .await,
            ),
            SubBkptType::Session(_) => None,
        };
        if group_operation.is_some() {
            sub_breakpoint = self
                .breakpoints
                .sub_breakpoint(breakpoint_id, sub_breakpoint_id)
                .ok_or_else(|| {
                    anyhow!(
                        "Sub-breakpoint {} of breakpoint {} disappeared while waiting for its group operation",
                        sub_breakpoint_id,
                        breakpoint_id
                    )
                })?;
        }

        match sub_breakpoint.kind() {
            SubBkptType::Session(session_breakpoint) => self
                .delete_local_breakpoint(
                    session_breakpoint.target_session(),
                    session_breakpoint.local_id(),
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to delete breakpoint {} from session {}",
                        session_breakpoint.local_id(),
                        session_breakpoint.target_session()
                    )
                }),
            SubBkptType::Group(group_breakpoint) => {
                let local_ids = group_breakpoint.local_ids();
                if local_ids.is_empty() {
                    return Ok(self.finalize_empty_sub_breakpoint(breakpoint_id, sub_breakpoint_id));
                }

                let mut change = BreakpointStateChange::None;
                let mut failed = false;
                for (session_id, local_breakpoint_id) in local_ids {
                    match self
                        .delete_local_breakpoint(session_id, local_breakpoint_id)
                        .await
                    {
                        Ok(local_change) => {
                            change = merge_state_changes(change, local_change);
                        }
                        Err(error) => {
                            failed = true;
                            error!(
                                session_id,
                                local_breakpoint_id,
                                %error,
                                "failed to delete group breakpoint target"
                            );
                        }
                    }
                }
                if failed {
                    bail!(
                        "Failed to delete some breakpoints from group sub-breakpoint {}",
                        sub_breakpoint_id
                    );
                }
                Ok(change)
            }
        }
    }

    fn finalize_empty_sub_breakpoint(
        &self,
        breakpoint_id: u64,
        sub_breakpoint_id: u64,
    ) -> BreakpointStateChange {
        self.breakpoints
            .remove_sub_breakpoint(breakpoint_id, sub_breakpoint_id)
    }

    async fn explicit_delete_outcome(
        &self,
        external_token: Option<u64>,
        change: BreakpointStateChange,
    ) -> Result<CommandOutcome> {
        match change {
            BreakpointStateChange::TargetChanged(breakpoint) => {
                let snapshot = BreakpointSnapshot::from(&breakpoint);
                let record = MIFormatter::format(
                    "=",
                    "breakpoint-modified",
                    Some(&bkpt_payload(&snapshot)),
                    None,
                );
                self.events
                    .broadcast(BreakpointChangeEvent::Updated(snapshot))
                    .await;

                let mut outcome =
                    CommandOutcome::completed(external_token, 0, "done", None, Presentation::Plain);
                outcome.push_record(record);
                Ok(outcome)
            }
            BreakpointStateChange::Removed(breakpoint_id) => {
                let record = MIFormatter::format(
                    "=",
                    "breakpoint-deleted",
                    Some(&bkpt_deleted_payload(breakpoint_id)),
                    None,
                );
                self.events
                    .broadcast(BreakpointChangeEvent::Removed(breakpoint_id))
                    .await;

                let mut outcome =
                    CommandOutcome::completed(external_token, 0, "done", None, Presentation::Plain);
                outcome.push_record(record);
                Ok(outcome)
            }
            BreakpointStateChange::None => Ok(CommandOutcome::empty()),
        }
    }
}

impl fmt::Debug for BreakpointService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("BreakpointService").finish()
    }
}

fn parse_breakpoint_location(args: &str) -> Result<BkptLoc> {
    let location = args
        .trim()
        .rsplit_once(char::is_whitespace)
        .map(|(_, tail)| tail)
        .unwrap_or(args)
        .trim_matches(['"', '\'']);
    let (source, line) = location.rsplit_once(':').ok_or_else(|| {
        anyhow!(
            "Unsupported breakpoint location '{}'. Expected <file>:<line>.",
            location
        )
    })?;
    if source.is_empty() {
        bail!("Breakpoint source path cannot be empty");
    }
    let line = line
        .parse::<u64>()
        .map_err(|_| anyhow!("Invalid breakpoint line '{}'", line))?;
    Ok(BkptLoc::new(source, line))
}

fn deduplicate_insertion_targets(groups: &GroupMgr, targets: &[Target]) -> Vec<Target> {
    let covered_sessions = targets
        .iter()
        .filter_map(|target| match target {
            Target::Group(group_id) => groups
                .group_by_id(*group_id)
                .map(|group| group.session_ids().clone()),
            _ => None,
        })
        .flatten()
        .collect::<HashSet<_>>();
    let mut seen_sessions = HashSet::new();
    let mut seen_groups = HashSet::new();

    targets
        .iter()
        .filter_map(|target| match target {
            Target::Session(session_id)
                if !covered_sessions.contains(session_id) && seen_sessions.insert(*session_id) =>
            {
                Some(Target::Session(*session_id))
            }
            Target::Group(group_id) if seen_groups.insert(*group_id) => {
                Some(Target::Group(*group_id))
            }
            _ => None,
        })
        .collect()
}

fn merge_state_changes(
    current: BreakpointStateChange,
    next: BreakpointStateChange,
) -> BreakpointStateChange {
    match (current, next) {
        (BreakpointStateChange::Removed(breakpoint_id), _)
        | (_, BreakpointStateChange::Removed(breakpoint_id)) => {
            BreakpointStateChange::Removed(breakpoint_id)
        }
        (BreakpointStateChange::TargetChanged(snapshot), _)
        | (_, BreakpointStateChange::TargetChanged(snapshot)) => {
            BreakpointStateChange::TargetChanged(snapshot)
        }
        _ => BreakpointStateChange::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_location_uses_the_final_argument_and_preserves_colons_in_paths() {
        let location = parse_breakpoint_location("-f \"C:/workspace/service.rs:42\"").unwrap();

        assert_eq!(location.path(), "C:/workspace/service.rs");
        assert_eq!(location.line(), 42);
    }

    #[test]
    fn breakpoint_location_rejects_missing_or_invalid_lines() {
        assert!(parse_breakpoint_location("service.rs").is_err());
        assert!(parse_breakpoint_location("service.rs:not-a-line").is_err());
        assert!(parse_breakpoint_location(":10").is_err());
    }

    #[test]
    fn insertion_target_plan_deduplicates_and_prefers_groups() {
        let groups = GroupMgr::new();
        groups.register_session("group-a", "service-a".to_string(), 11);
        groups.register_session("group-a", "service-b".to_string(), 12);
        let group_id = groups.group_id_by_session(11).unwrap();

        let plan = deduplicate_insertion_targets(
            &groups,
            &[
                Target::Session(11),
                Target::Group(group_id),
                Target::Group(group_id),
                Target::Session(12),
                Target::Session(13),
                Target::Session(13),
                Target::Broadcast,
            ],
        );

        assert_eq!(plan, vec![Target::Group(group_id), Target::Session(13)]);
    }

    #[test]
    fn removed_state_change_dominates_target_updates() {
        let manager = BreakpointMgr::new();
        let breakpoint_id = manager.add_breakpoint(BkptLoc::new("main.rs", 10));
        let snapshot = manager.breakpoint(breakpoint_id).unwrap();
        let merged = merge_state_changes(
            BreakpointStateChange::TargetChanged(snapshot),
            BreakpointStateChange::Removed(breakpoint_id),
        );

        assert!(matches!(
            merged,
            BreakpointStateChange::Removed(id) if id == breakpoint_id
        ));
    }
}
