use std::{collections::HashSet, fmt, sync::Arc};

use anyhow::{anyhow, bail, Context, Result};
use tracing::{error, warn};

use crate::{
    notification::{BreakpointChangeEvent, Notification, NotificationManager, NotificationPayload},
    state::{
        BkptLoc, BreakpointSnapshot, BreakpointStateChange, GroupId, RuntimeModel, SubBkptSpec,
        SubBkptType,
    },
};

use super::{
    api::CommandExecutor,
    breakpoint_mi::{bkpt_deleted_payload, bkpt_payload},
    decoder::BreakpointCreated,
    event::{ProjectedDebuggerOutput, ProjectedDebuggerRecord},
    event_publisher::EventPublisher,
    input::ParsedInputCmd,
    mi::MiFormatter,
    router::Target,
    CommandOutcome, Presentation,
};

/// Sole boundary for breakpoint notifications and automatic MI records.
///
/// Records flow through the shared asynchronous record sink; nothing here
/// writes to stdout directly.
pub(crate) struct BreakpointEventPublisher {
    notifications: Arc<NotificationManager>,
    records: EventPublisher,
}

impl BreakpointEventPublisher {
    pub(crate) fn new(
        notifications: Arc<NotificationManager>,
        records: EventPublisher,
    ) -> Arc<Self> {
        Arc::new(Self {
            notifications,
            records,
        })
    }

    pub(crate) async fn publish_state_change(&self, change: BreakpointStateChange) {
        let event = match change {
            BreakpointStateChange::None => return,
            BreakpointStateChange::TargetChanged(breakpoint) => {
                let snapshot = BreakpointSnapshot::from(&breakpoint);
                self.publish_record("breakpoint-modified", bkpt_payload(&snapshot))
                    .await;
                BreakpointChangeEvent::TargetChanged(snapshot.id)
            }
            BreakpointStateChange::Removed(breakpoint_id) => {
                self.publish_record("breakpoint-deleted", bkpt_deleted_payload(breakpoint_id))
                    .await;
                BreakpointChangeEvent::Removed(breakpoint_id)
            }
        };
        self.broadcast(event).await;
    }

    async fn publish_record(&self, message: &str, payload: crate::debugger::protocol::Dict) {
        let output = ProjectedDebuggerOutput {
            records: vec![ProjectedDebuggerRecord {
                prefix: "=",
                message: message.to_string(),
                payload: Some(payload),
                token: None,
            }],
        };
        if let Err(error) = self.records.publish(output).await {
            warn!(%message, ?error, "failed to publish breakpoint record");
        }
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
    model: Arc<RuntimeModel>,
    events: Arc<BreakpointEventPublisher>,
    executor: CommandExecutor,
}

impl BreakpointService {
    pub(crate) fn new(
        model: Arc<RuntimeModel>,
        events: Arc<BreakpointEventPublisher>,
        executor: CommandExecutor,
    ) -> Self {
        Self {
            model,
            events,
            executor,
        }
    }

    fn group_ids_for_breakpoint(&self, breakpoint_id: u64) -> Vec<GroupId> {
        self.model
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
        let planned_targets = match &command.target {
            Target::Multiple(targets) => {
                deduplicate_insertion_targets(self.model.as_ref(), targets)
            }
            single => vec![single.clone()],
        };
        let lenient = matches!(&command.target, Target::Multiple(_));
        let group_gates = self
            .model
            .lock_group_operations(planned_targets.iter().filter_map(|target| match target {
                Target::Group(group_id) => Some(*group_id),
                _ => None,
            }))
            .await;

        let mut specs = Vec::new();
        for target in planned_targets {
            let spec = match target {
                Target::Session(session_id) => self
                    .session_spec(&debugger_command, session_id)
                    .await
                    .map_err(|error| {
                        anyhow!(
                            "Failed to insert breakpoint into session {}: {}",
                            session_id,
                            error
                        )
                    }),
                Target::Group(group_id) => self
                    .group_spec(&debugger_command, group_id)
                    .await
                    .map_err(|error| {
                        anyhow!(
                            "Failed to insert breakpoint into group {}: {}",
                            group_id,
                            error
                        )
                    }),
                _ => unreachable!("target planning only returns session and group targets"),
            };
            match spec {
                Ok(spec) => specs.push(spec),
                Err(error) if lenient => {
                    warn!(%error, "failed to insert breakpoint into one target");
                }
                Err(error) => return Err(error),
            }
        }

        let Some(breakpoint) = self.model.insert_breakpoint(location, specs) else {
            bail!("Failed to insert breakpoint into any target.");
        };
        drop(group_gates);

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

    /// Runs the insertion on one session and reports what it contributed.
    async fn session_spec(&self, debugger_command: &str, session_id: u64) -> Result<SubBkptSpec> {
        let completion = self
            .executor
            .execute(debugger_command, Target::Session(session_id))
            .await?;
        let breakpoint = BreakpointCreated::decode_first(&completion)?;
        let _ = breakpoint.times;
        Ok(SubBkptSpec::Session {
            sid: session_id,
            local_id: breakpoint.local_id,
        })
    }

    /// Runs the insertion on every active session of a group. A group without
    /// active sessions yields a spec with no locals so late joiners can still
    /// be attached. The caller holds the group's operation gate.
    async fn group_spec(&self, debugger_command: &str, group_id: GroupId) -> Result<SubBkptSpec> {
        let group = self
            .model
            .group_by_id(group_id)
            .ok_or_else(|| anyhow!("Group {} does not exist", group_id))?;
        let mut locals = Vec::new();
        if !group.session_ids().is_empty() {
            let completion = self
                .executor
                .execute(debugger_command, Target::Group(group_id))
                .await?;
            for response in completion.get_responses() {
                let breakpoint = BreakpointCreated::decode(response)?;
                let _ = breakpoint.times;
                locals.push((response.get_sid(), breakpoint.local_id));
            }
        }
        Ok(SubBkptSpec::Group { group_id, locals })
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
            .model
            .lock_group_operations(self.group_ids_for_breakpoint(breakpoint_id))
            .await;
        for (session_id, local_breakpoint_id) in self.model.local_breakpoint_ids(breakpoint_id) {
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

        self.model.remove_breakpoint(breakpoint_id);
        drop(group_operations);
        let record = MiFormatter::format(
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
            .model
            .record_local_breakpoint_deletion(session_id, local_breakpoint_id))
    }

    async fn delete_sub_breakpoint(
        &self,
        breakpoint_id: u64,
        sub_breakpoint_id: u64,
    ) -> Result<BreakpointStateChange> {
        let mut sub_breakpoint = self
            .model
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
                self.model
                    .lock_group_operation(group_breakpoint.target_group())
                    .await,
            ),
            SubBkptType::Session(_) => None,
        };
        if group_operation.is_some() {
            sub_breakpoint = self
                .model
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
        self.model
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
                let record = MiFormatter::format(
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
                let record = MiFormatter::format(
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

fn deduplicate_insertion_targets(model: &RuntimeModel, targets: &[Target]) -> Vec<Target> {
    let covered_sessions = targets
        .iter()
        .filter_map(|target| match target {
            Target::Group(group_id) => model
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

    #[tokio::test]
    async fn insertion_target_plan_deduplicates_and_prefers_groups() {
        let model = RuntimeModel::new();
        let identity = crate::state::ServiceIdentity::new("group-a", "service-a");
        model
            .register_session(11, "service-a", Some(identity.clone()))
            .await;
        drop(model.register_service_group(11, &identity).await);
        model
            .register_session(12, "service-b", Some(identity.clone()))
            .await;
        drop(model.register_service_group(12, &identity).await);
        let group_id = model.group_id_by_session(11).unwrap();

        let plan = deduplicate_insertion_targets(
            &model,
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
        let model = RuntimeModel::new();
        let breakpoint_id = model.add_breakpoint(BkptLoc::new("main.rs", 10));
        let snapshot = model.breakpoint(breakpoint_id).unwrap();
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
