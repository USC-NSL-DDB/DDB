use std::{collections::HashSet, fmt, sync::Arc};

use anyhow::{anyhow, bail, Context, Result};
use tokio::sync::Mutex;
use tracing::{error, warn};

use crate::{
    notification::{
        BreakpointChangeEvent, DebuggerOutputEvent, DebuggerOutputRecord, Notification,
        NotificationManager, NotificationPayload,
    },
    state::{
        BkptLoc, BkptMeta, BreakpointProperties, BreakpointSnapshot, BreakpointStateChange,
        GroupId, RuntimeModel, SubBkptSpec, SubBkptType,
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
    output_hub::{DebuggerOutputStream, OutputHub},
    router::{
        CommandFanoutError, CommandFanoutReport, SessionCommandFailure, SessionCommandFailureKind,
        Target,
    },
    CommandOutcome, ParsedSessionResponse, Presentation,
};

/// Sole boundary for breakpoint notifications and automatic MI records.
///
/// Records flow through the shared asynchronous record sink; nothing here
/// writes to stdout directly.
pub(crate) struct BreakpointEventPublisher {
    notifications: Arc<NotificationManager>,
    records: EventPublisher,
    output: Arc<OutputHub>,
}

impl BreakpointEventPublisher {
    pub(crate) fn new(
        notifications: Arc<NotificationManager>,
        records: EventPublisher,
        output: Arc<OutputHub>,
    ) -> Arc<Self> {
        Arc::new(Self {
            notifications,
            records,
            output,
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
                stream: None,
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

    pub(crate) async fn publish_update(&self, breakpoint: BkptMeta) {
        let snapshot = BreakpointSnapshot::from(&breakpoint);
        self.publish_record("breakpoint-modified", bkpt_payload(&snapshot))
            .await;
        self.broadcast(BreakpointChangeEvent::Updated(snapshot))
            .await;
    }

    pub(crate) async fn broadcast(&self, event: BreakpointChangeEvent) {
        self.notifications
            .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                event,
            )))
            .await;
    }

    pub(crate) async fn broadcast_debugger_output(
        &self,
        sid: u64,
        output: &ProjectedDebuggerOutput,
    ) {
        for record in &output.records {
            let Some(stream) = record.stream.and_then(debugger_output_stream) else {
                continue;
            };
            if let Err(error) = self
                .output
                .publish(Some(sid), stream, record.message.clone())
            {
                warn!(sid, ?error, "failed to publish typed debugger output");
            }
        }
        let records = output
            .records
            .iter()
            .map(|record| {
                let stream = record.stream.unwrap_or(match record.prefix {
                    "*" => "exec",
                    "=" => "notify",
                    "+" => "status",
                    "~" => "console",
                    "@" => "target",
                    "&" => "log",
                    other => other,
                });
                let (event, payload) = if record.stream.is_some() {
                    (
                        "output".to_string(),
                        Some(serde_json::json!({"message": record.message})),
                    )
                } else {
                    (
                        record.message.clone(),
                        record
                            .payload
                            .as_ref()
                            .map(crate::api::contract::dict_to_json),
                    )
                };
                DebuggerOutputRecord {
                    stream: stream.to_string(),
                    event,
                    payload,
                    token: record.token,
                }
            })
            .collect();
        self.notifications
            .broadcast(Notification::new(NotificationPayload::DebuggerOutput(
                DebuggerOutputEvent { records },
            )))
            .await;
    }
}

fn debugger_output_stream(stream: &str) -> Option<DebuggerOutputStream> {
    match stream {
        "console" => Some(DebuggerOutputStream::Console),
        "log" => Some(DebuggerOutputStream::Log),
        "target" => Some(DebuggerOutputStream::Target),
        "inferior_stdout" => Some(DebuggerOutputStream::InferiorStdout),
        "inferior_stderr" => Some(DebuggerOutputStream::InferiorStderr),
        "prompt" => Some(DebuggerOutputStream::Prompt),
        _ => None,
    }
}

pub(crate) struct BreakpointService {
    model: Arc<RuntimeModel>,
    events: Arc<BreakpointEventPublisher>,
    executor: CommandExecutor,
    operations: Mutex<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConditionUpdate {
    Keep,
    Replace(Option<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BreakpointUpdate {
    enabled: Option<bool>,
    condition: ConditionUpdate,
}

#[derive(Default)]
struct LocalInsertionReport {
    locals: Vec<(u64, u64)>,
    failures: Vec<SessionCommandFailure>,
}

impl BreakpointUpdate {
    fn enabled(enabled: bool) -> Self {
        Self {
            enabled: Some(enabled),
            condition: ConditionUpdate::Keep,
        }
    }

    fn condition(condition: Option<String>) -> Self {
        Self {
            enabled: None,
            condition: ConditionUpdate::Replace(condition),
        }
    }
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
            operations: Mutex::new(()),
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
        let _operation = self.operations.lock().await;
        if !matches!(
            &command.target,
            Target::Session(_) | Target::Group(_) | Target::Multiple(_)
        ) {
            bail!("break-insert requires a session, group, or multiple target");
        }

        let debugger_command = command.full_cmd();
        let (location, properties) = parse_breakpoint_definition(&command.args)?;
        let planned_targets = match &command.target {
            Target::Multiple(targets) => {
                deduplicate_insertion_targets(self.model.as_ref(), targets)
            }
            single => vec![single.clone()],
        };
        let group_gates = self
            .model
            .lock_group_operations(planned_targets.iter().filter_map(|target| match target {
                Target::Group(group_id) => Some(*group_id),
                _ => None,
            }))
            .await;

        let planned_targets = planned_targets
            .into_iter()
            .map(|target| {
                let session_ids = self.executor.resolve_session_ids(&target)?;
                Ok((target, session_ids))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut specs = Vec::new();
        let mut successful_sessions = HashSet::new();
        let mut failures = Vec::new();
        for (target, expected_sessions) in planned_targets {
            if expected_sessions.is_empty() {
                if let Target::Group(group_id) = target {
                    specs.push(SubBkptSpec::Group {
                        group_id,
                        locals: Vec::new(),
                    });
                }
                continue;
            }
            let report = self
                .insert_local_breakpoints(&debugger_command, target.clone(), &expected_sessions)
                .await;
            successful_sessions.extend(report.locals.iter().map(|(sid, _)| *sid));
            failures.extend(report.failures);
            match target {
                Target::Session(session_id) => {
                    if let Some((_, local_id)) =
                        report.locals.iter().find(|(sid, _)| *sid == session_id)
                    {
                        specs.push(SubBkptSpec::Session {
                            sid: session_id,
                            local_id: *local_id,
                        });
                    }
                }
                Target::Group(group_id) => {
                    if !report.locals.is_empty() {
                        specs.push(SubBkptSpec::Group {
                            group_id,
                            locals: report.locals,
                        });
                    }
                }
                _ => unreachable!("target planning only returns session and group targets"),
            }
        }

        failures.sort_unstable_by_key(SessionCommandFailure::sid);
        failures.dedup_by_key(|failure| failure.sid());
        if successful_sessions.is_empty() && !failures.is_empty() {
            return CommandFanoutReport::new(command.external_token, Vec::new(), failures)
                .into_result()
                .map(|completion| CommandOutcome::response(completion, Presentation::Plain));
        }
        let Some(breakpoint) = self.model.insert_breakpoint(location, properties, specs) else {
            bail!("Failed to insert breakpoint into any target.");
        };
        drop(group_gates);

        let snapshot = BreakpointSnapshot::from(&breakpoint);
        let payload = bkpt_payload(&snapshot);
        self.events
            .broadcast(BreakpointChangeEvent::Added(snapshot))
            .await;

        let mut successful_sessions = successful_sessions.into_iter().collect::<Vec<_>>();
        successful_sessions.sort_unstable();
        let responses = if successful_sessions.is_empty() {
            vec![ParsedSessionResponse::new(
                0,
                "done".to_string(),
                Some(payload),
            )]
        } else {
            successful_sessions
                .into_iter()
                .enumerate()
                .map(|(index, sid)| {
                    ParsedSessionResponse::new(
                        sid,
                        "done".to_string(),
                        (index == 0).then(|| payload.clone()),
                    )
                })
                .collect()
        };
        let completion =
            CommandFanoutReport::new(command.external_token, responses, failures).into_result()?;
        Ok(CommandOutcome::response(completion, Presentation::Plain))
    }

    async fn insert_local_breakpoints(
        &self,
        debugger_command: &str,
        target: Target,
        expected_sessions: &[u64],
    ) -> LocalInsertionReport {
        let (responses, mut failures) = match self.executor.execute(debugger_command, target).await
        {
            Ok(completion) => (completion.get_responses().clone(), Vec::new()),
            Err(error) => match error.downcast_ref::<CommandFanoutError>() {
                Some(fanout) => (
                    fanout.report().completion().get_responses().clone(),
                    fanout.report().failures().to_vec(),
                ),
                None => (
                    Vec::new(),
                    expected_sessions
                        .iter()
                        .copied()
                        .map(|sid| {
                            SessionCommandFailure::new(
                                sid,
                                SessionCommandFailureKind::ExecutionFailed,
                            )
                        })
                        .collect(),
                ),
            },
        };
        let mut locals = Vec::new();
        for response in responses {
            let sid = response.get_sid();
            if response.get_message() == "error" {
                failures.push(SessionCommandFailure::new(
                    sid,
                    SessionCommandFailureKind::DebuggerRejected,
                ));
                continue;
            }
            match BreakpointCreated::decode(&response) {
                Ok(breakpoint) => {
                    let _ = breakpoint.times;
                    locals.push((sid, breakpoint.local_id));
                }
                Err(_) => {
                    warn!(
                        sid,
                        "debugger returned a malformed successful breakpoint insertion response"
                    );
                    failures.push(SessionCommandFailure::new(
                        sid,
                        SessionCommandFailureKind::ExecutionFailed,
                    ));
                }
            }
        }
        LocalInsertionReport { locals, failures }
    }

    pub(crate) async fn set_enabled(
        &self,
        command: ParsedInputCmd,
        enabled: bool,
    ) -> Result<CommandOutcome> {
        let _operation = self.operations.lock().await;
        let breakpoint_ids = command
            .args
            .split_whitespace()
            .map(|value| {
                value
                    .parse::<u64>()
                    .with_context(|| format!("Invalid breakpoint id {value}"))
            })
            .collect::<Result<Vec<_>>>()?;
        if breakpoint_ids.is_empty() {
            bail!("No breakpoint id provided.");
        }

        for breakpoint_id in breakpoint_ids {
            self.update_one(breakpoint_id, BreakpointUpdate::enabled(enabled))
                .await?;
        }
        Ok(breakpoint_update_outcome(command.external_token))
    }

    pub(crate) async fn set_condition(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let arguments = split_quoted_arguments(&command.args)?;
        let breakpoint_id = arguments
            .first()
            .ok_or_else(|| anyhow!("No breakpoint id provided."))?
            .parse::<u64>()
            .context("Invalid breakpoint id")?;
        let condition = if arguments.len() > 1 {
            Some(arguments[1..].join(" "))
        } else {
            None
        };

        let _operation = self.operations.lock().await;
        self.update_one(breakpoint_id, BreakpointUpdate::condition(condition))
            .await?;
        Ok(breakpoint_update_outcome(command.external_token))
    }

    pub(crate) async fn update(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let (breakpoint_id, update) = parse_breakpoint_update(&command.args)?;
        let _operation = self.operations.lock().await;
        self.update_one(breakpoint_id, update).await?;
        Ok(breakpoint_update_outcome(command.external_token))
    }

    async fn update_one(&self, breakpoint_id: u64, update: BreakpointUpdate) -> Result<()> {
        let breakpoint = self
            .model
            .breakpoint(breakpoint_id)
            .ok_or_else(|| anyhow!("Breakpoint {} does not exist", breakpoint_id))?;
        let previous_enabled = breakpoint.is_enabled();
        let previous_condition = breakpoint.properties().condition.clone();
        let enabled = update.enabled.unwrap_or(previous_enabled);
        let condition = match update.condition {
            ConditionUpdate::Keep => previous_condition.clone(),
            ConditionUpdate::Replace(condition) => condition,
        };
        let update_enabled = enabled != previous_enabled;
        let update_condition = condition != previous_condition;
        if !update_enabled && !update_condition {
            return Ok(());
        }

        let group_operations = self
            .model
            .lock_group_operations(self.group_ids_for_breakpoint(breakpoint_id))
            .await;
        let mut local_ids = self.model.local_breakpoint_ids(breakpoint_id);
        local_ids.sort_unstable();
        let mut enabled_locals = Vec::new();
        if update_enabled {
            for &(session_id, local_breakpoint_id) in &local_ids {
                if let Err(error) = self
                    .set_local_enabled(enabled, session_id, local_breakpoint_id)
                    .await
                {
                    self.rollback_local_enabled(breakpoint_id, &enabled_locals, previous_enabled)
                        .await;
                    return Err(error).with_context(|| {
                        format!(
                            "Failed to {} breakpoint {}",
                            if enabled { "enable" } else { "disable" },
                            breakpoint_id
                        )
                    });
                }
                enabled_locals.push((session_id, local_breakpoint_id));
            }
        }

        let mut condition_locals = Vec::new();
        if update_condition {
            for &(session_id, local_breakpoint_id) in &local_ids {
                if let Err(error) = self
                    .set_local_condition(session_id, local_breakpoint_id, condition.as_deref())
                    .await
                {
                    self.rollback_local_conditions(
                        breakpoint_id,
                        &condition_locals,
                        previous_condition.as_deref(),
                    )
                    .await;
                    self.rollback_local_enabled(breakpoint_id, &enabled_locals, previous_enabled)
                        .await;
                    return Err(error).with_context(|| {
                        format!("Failed to update condition for breakpoint {breakpoint_id}")
                    });
                }
                condition_locals.push((session_id, local_breakpoint_id));
            }
        }

        let breakpoint = self
            .model
            .update_breakpoint(breakpoint_id, enabled, condition)
            .ok_or_else(|| anyhow!("Breakpoint {} disappeared during update", breakpoint_id))?;
        drop(group_operations);
        self.events.publish_update(breakpoint).await;
        Ok(())
    }

    async fn rollback_local_enabled(
        &self,
        breakpoint_id: u64,
        local_ids: &[(u64, u64)],
        enabled: bool,
    ) {
        for &(session_id, local_breakpoint_id) in local_ids.iter().rev() {
            if let Err(rollback_error) = self
                .set_local_enabled(enabled, session_id, local_breakpoint_id)
                .await
            {
                error!(
                    breakpoint_id,
                    session_id,
                    local_breakpoint_id,
                    %rollback_error,
                    "failed to roll back partial breakpoint enabled update"
                );
            }
        }
    }

    async fn rollback_local_conditions(
        &self,
        breakpoint_id: u64,
        local_ids: &[(u64, u64)],
        condition: Option<&str>,
    ) {
        for &(session_id, local_breakpoint_id) in local_ids.iter().rev() {
            if let Err(rollback_error) = self
                .set_local_condition(session_id, local_breakpoint_id, condition)
                .await
            {
                error!(
                    breakpoint_id,
                    session_id,
                    local_breakpoint_id,
                    %rollback_error,
                    "failed to roll back partial breakpoint condition update"
                );
            }
        }
    }

    async fn set_local_enabled(
        &self,
        enabled: bool,
        session_id: u64,
        local_breakpoint_id: u64,
    ) -> Result<()> {
        let operation = if enabled {
            "-break-enable"
        } else {
            "-break-disable"
        };
        self.execute_local_update(
            session_id,
            local_breakpoint_id,
            format!("{operation} {local_breakpoint_id}"),
        )
        .await
    }

    async fn set_local_condition(
        &self,
        session_id: u64,
        local_breakpoint_id: u64,
        condition: Option<&str>,
    ) -> Result<()> {
        let condition = condition
            .map(|value| {
                format!(
                    " {}",
                    serde_json::to_string(value).expect("serializing a string cannot fail")
                )
            })
            .unwrap_or_default();
        self.execute_local_update(
            session_id,
            local_breakpoint_id,
            format!("-break-condition {local_breakpoint_id}{condition}"),
        )
        .await
    }

    async fn execute_local_update(
        &self,
        session_id: u64,
        local_breakpoint_id: u64,
        command: String,
    ) -> Result<()> {
        let completion = self
            .executor
            .execute(&command, Target::Session(session_id))
            .await?;
        let response = completion.get_responses().first().ok_or_else(|| {
            anyhow!(
                "Debugger returned no response for breakpoint {} in session {}",
                local_breakpoint_id,
                session_id
            )
        })?;
        if response.get_message() != "done" {
            bail!(
                "Debugger returned {} for breakpoint {} in session {}",
                response.get_message(),
                local_breakpoint_id,
                session_id
            );
        }
        Ok(())
    }

    pub(crate) async fn delete(&self, command: ParsedInputCmd) -> Result<CommandOutcome> {
        let _operation = self.operations.lock().await;
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
        let mut local_ids = self.model.local_breakpoint_ids(breakpoint_id);
        local_ids.sort_unstable();
        let mut successful_sessions = Vec::new();
        let mut failures = Vec::new();
        let mut change = BreakpointStateChange::None;
        for (session_id, local_breakpoint_id) in local_ids {
            match self
                .delete_local_breakpoint(session_id, local_breakpoint_id)
                .await
            {
                Ok(local_change) => {
                    successful_sessions.push(session_id);
                    change = merge_state_changes(change, local_change);
                }
                Err(_) => failures.push(SessionCommandFailure::new(
                    session_id,
                    SessionCommandFailureKind::ExecutionFailed,
                )),
            }
        }

        if failures.is_empty() {
            self.model.remove_breakpoint(breakpoint_id);
            change = BreakpointStateChange::Removed(breakpoint_id);
        }
        drop(group_operations);
        self.events.publish_state_change(change).await;

        let responses = if failures.is_empty() {
            vec![ParsedSessionResponse::new(0, "done".to_string(), None)]
        } else {
            successful_sessions
                .into_iter()
                .map(|sid| ParsedSessionResponse::new(sid, "done".to_string(), None))
                .collect()
        };
        let completion =
            CommandFanoutReport::new(command.external_token, responses, failures).into_result()?;
        let record = MiFormatter::format(
            "=",
            "breakpoint-deleted",
            Some(&bkpt_deleted_payload(breakpoint_id)),
            None,
        );
        let mut outcome = CommandOutcome::response(completion, Presentation::Plain);
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

fn breakpoint_update_outcome(external_token: Option<u64>) -> CommandOutcome {
    CommandOutcome::completed(external_token, 0, "done", None, Presentation::Plain)
}

fn parse_breakpoint_update(args: &str) -> Result<(u64, BreakpointUpdate)> {
    let arguments = split_quoted_arguments(args)?;
    let breakpoint_id = arguments
        .first()
        .ok_or_else(|| anyhow!("No breakpoint id provided."))?
        .parse::<u64>()
        .context("Invalid breakpoint id")?;
    let mut update = BreakpointUpdate {
        enabled: None,
        condition: ConditionUpdate::Keep,
    };
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--enabled" => {
                if update.enabled.is_some() {
                    bail!("Duplicate --enabled breakpoint update");
                }
                let value = arguments
                    .get(index + 1)
                    .ok_or_else(|| anyhow!("--enabled requires true or false"))?;
                update.enabled = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => bail!("--enabled requires true or false"),
                });
                index += 2;
            }
            "--condition" => {
                if !matches!(&update.condition, ConditionUpdate::Keep) {
                    bail!("Duplicate breakpoint condition update");
                }
                let value = arguments
                    .get(index + 1)
                    .ok_or_else(|| anyhow!("--condition requires an expression"))?;
                update.condition = ConditionUpdate::Replace(Some(value.clone()));
                index += 2;
            }
            "--clear-condition" => {
                if !matches!(&update.condition, ConditionUpdate::Keep) {
                    bail!("Duplicate breakpoint condition update");
                }
                update.condition = ConditionUpdate::Replace(None);
                index += 1;
            }
            option => bail!("Unknown breakpoint update option {option}"),
        }
    }
    if update.enabled.is_none() && matches!(&update.condition, ConditionUpdate::Keep) {
        bail!("No breakpoint fields were provided for update");
    }
    Ok((breakpoint_id, update))
}

pub(crate) fn breakpoint_insert_command(
    location: &BkptLoc,
    properties: &BreakpointProperties,
) -> String {
    let mut arguments = Vec::new();
    if !properties.enabled {
        arguments.push("-d".to_string());
    }
    if properties.temporary {
        arguments.push("-t".to_string());
    }
    if properties.hardware {
        arguments.push("-h".to_string());
    }
    if let Some(condition) = properties
        .condition
        .as_deref()
        .filter(|condition| !condition.trim().is_empty())
    {
        arguments.push("-c".to_string());
        arguments.push(serde_json::to_string(condition).expect("serializing a string cannot fail"));
    }
    arguments.push(
        serde_json::to_string(&location.breakpoint_path())
            .expect("serializing a string cannot fail"),
    );
    format!("-break-insert {}", arguments.join(" "))
}

#[cfg(test)]
fn parse_breakpoint_location(args: &str) -> Result<BkptLoc> {
    parse_breakpoint_definition(args).map(|(location, _)| location)
}

fn parse_breakpoint_definition(args: &str) -> Result<(BkptLoc, BreakpointProperties)> {
    let arguments = split_quoted_arguments(args)?;
    let location = arguments
        .last()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("Breakpoint location is missing"))?;
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
    let mut properties = BreakpointProperties::default();
    let mut index = 0;
    while index + 1 < arguments.len() {
        match arguments[index].as_str() {
            "-d" | "--disabled" => properties.enabled = false,
            "-t" | "--temporary" => properties.temporary = true,
            "-h" | "--hardware" => properties.hardware = true,
            "-c" | "--condition" if index + 1 < arguments.len() - 1 => {
                index += 1;
                properties.condition = Some(arguments[index].clone());
            }
            _ => {}
        }
        index += 1;
    }
    Ok((BkptLoc::new(source, line), properties))
}

fn split_quoted_arguments(input: &str) -> Result<Vec<String>> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(delimiter) if character == delimiter => quote = None,
            Some(_) => current.push(character),
            None if matches!(character, '"' | '\'') => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    arguments.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if escaped {
        current.push('\\');
    }
    if let Some(delimiter) = quote {
        bail!("Unterminated {delimiter} quote in breakpoint arguments");
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
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
    fn breakpoint_definition_preserves_advanced_properties() {
        let (location, properties) =
            parse_breakpoint_definition(r#"-t -c "request.id == 42" "C:/workspace/service.rs:42""#)
                .unwrap();

        assert_eq!(location.path(), "C:/workspace/service.rs");
        assert_eq!(location.line(), 42);
        assert_eq!(properties.condition.as_deref(), Some("request.id == 42"));
        assert!(properties.temporary);
        assert!(!properties.hardware);
    }

    #[test]
    fn breakpoint_insert_command_round_trips_properties_and_location() {
        let properties = BreakpointProperties {
            enabled: false,
            condition: Some("request.id == \"special\"".to_string()),
            temporary: true,
            hardware: false,
        };
        let command = breakpoint_insert_command(
            &BkptLoc::new("C:/workspace/service main.rs", 42),
            &properties,
        );
        assert_eq!(
            command,
            r#"-break-insert -d -t -c "request.id == \"special\"" "C:/workspace/service main.rs:42""#
        );
        let (_, parsed) = parse_breakpoint_definition(
            command
                .strip_prefix("-break-insert ")
                .expect("builder emits the command prefix"),
        )
        .unwrap();
        assert_eq!(parsed, properties);
    }

    #[test]
    fn breakpoint_update_parser_supports_atomic_enabled_and_condition_changes() {
        let (breakpoint_id, update) = parse_breakpoint_update(
            r#"42 --enabled false --condition "request.id == \"special\"""#,
        )
        .unwrap();
        assert_eq!(breakpoint_id, 42);
        assert_eq!(
            update,
            BreakpointUpdate {
                enabled: Some(false),
                condition: ConditionUpdate::Replace(Some("request.id == \"special\"".to_string())),
            }
        );

        assert_eq!(
            parse_breakpoint_update("42 --clear-condition").unwrap().1,
            BreakpointUpdate::condition(None)
        );
        assert!(parse_breakpoint_update("42").is_err());
        assert!(parse_breakpoint_update("42 --enabled maybe").is_err());
        assert!(parse_breakpoint_update("42 --clear-condition --condition x").is_err());
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
