use std::collections::{BTreeMap, HashMap};

use ddb_api_types::v2::{self, resource_upsert, state_event};

use crate::{ClientError, Result};

/// Outcome of applying one state event to a [`StateProjection`].
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectionUpdate {
    Applied,
    /// The event or resource revision was already represented locally.
    IgnoredStale,
    /// The projection cannot safely continue and must be replaced by a new
    /// snapshot. The optional error is safe to display to a developer.
    RehydrationRequired(Option<Box<v2::DdbError>>),
}

/// Deterministic, idempotent frontend projection of snapshot and state events.
///
/// Collection maps are keyed by opaque public resource IDs. They are exposed
/// read-only so consumers cannot accidentally bypass revision checks.
#[derive(Clone, Debug, Default)]
pub struct StateProjection {
    server_instance_id: String,
    cursor: Option<v2::Cursor>,
    state_revision: u64,
    included_sections: Vec<i32>,
    sessions: BTreeMap<String, v2::Session>,
    groups: BTreeMap<String, v2::Group>,
    processes: BTreeMap<String, v2::Process>,
    threads: BTreeMap<String, v2::Thread>,
    selections: BTreeMap<String, v2::Selection>,
    execution_states: BTreeMap<String, v2::ExecutionState>,
    breakpoints: BTreeMap<String, v2::Breakpoint>,
    operations: BTreeMap<String, v2::Operation>,
    extension_states: BTreeMap<String, v2::ExtensionState>,
    pending_commands: BTreeMap<String, v2::PendingCommand>,
    capabilities: Option<v2::Capabilities>,
    tombstones: HashMap<(i32, String), u64>,
}

impl StateProjection {
    pub fn from_snapshot(snapshot: v2::Snapshot) -> Result<Self> {
        let mut projection = Self::default();
        projection.replace_snapshot(snapshot)?;
        Ok(projection)
    }

    /// Atomically replaces the entire derived projection with a fresh
    /// snapshot. Omitted sections remain empty by design.
    pub fn replace_snapshot(&mut self, snapshot: v2::Snapshot) -> Result<()> {
        let cursor = snapshot.state_event_cursor.clone().ok_or_else(|| {
            ClientError::Protocol("snapshot omitted state_event_cursor".to_string())
        })?;
        if snapshot.server_instance_id.is_empty()
            || cursor.server_instance_id != snapshot.server_instance_id
        {
            return Err(ClientError::Protocol(
                "snapshot and cursor server-instance identities disagree".to_string(),
            ));
        }

        if let Some(capabilities) = snapshot.capabilities.as_ref() {
            if capabilities.capabilities_id.is_empty()
                || capabilities.revision == 0
                || capabilities.server_instance_id != snapshot.server_instance_id
            {
                return Err(ClientError::Protocol(
                    "snapshot capabilities have invalid identity, revision, or server instance"
                        .to_string(),
                ));
            }
        }

        let mut next = Self {
            server_instance_id: snapshot.server_instance_id,
            cursor: Some(cursor),
            state_revision: snapshot.base_state_revision,
            included_sections: snapshot.included_sections,
            capabilities: snapshot.capabilities,
            ..Self::default()
        };
        insert_snapshot_resources(
            &mut next.sessions,
            snapshot.sessions,
            |item| (&item.session_id, item.revision),
            "session",
        )?;
        insert_snapshot_resources(
            &mut next.groups,
            snapshot.groups,
            |item| (&item.group_id, item.revision),
            "group",
        )?;
        insert_snapshot_resources(
            &mut next.processes,
            snapshot.processes,
            |item| (&item.process_id, item.revision),
            "process",
        )?;
        insert_snapshot_resources(
            &mut next.threads,
            snapshot.threads,
            |item| (&item.thread_id, item.revision),
            "thread",
        )?;
        if let Some(selection) = snapshot.selection {
            insert_snapshot_resource(
                &mut next.selections,
                selection.selection_id.clone(),
                selection.revision,
                selection,
                "selection",
            )?;
        }
        insert_snapshot_resources(
            &mut next.execution_states,
            snapshot.execution_states,
            |item| (&item.execution_state_id, item.revision),
            "execution state",
        )?;
        insert_snapshot_resources(
            &mut next.breakpoints,
            snapshot.breakpoints,
            |item| (&item.breakpoint_id, item.revision),
            "breakpoint",
        )?;
        insert_snapshot_resources(
            &mut next.operations,
            snapshot.operations,
            |item| (&item.operation_id, item.revision),
            "operation",
        )?;
        insert_snapshot_resources(
            &mut next.extension_states,
            snapshot.extension_states,
            |item| (&item.extension_state_id, item.revision),
            "extension state",
        )?;
        insert_snapshot_resources(
            &mut next.pending_commands,
            snapshot.pending_commands,
            |item| (&item.pending_command_id, item.revision),
            "pending command",
        )?;
        *self = next;
        Ok(())
    }

    /// Applies one replayed or live event. Duplicate cursors and stale resource
    /// revisions are harmless and return [`ProjectionUpdate::IgnoredStale`].
    pub fn apply_event(&mut self, mut event: v2::StateEvent) -> Result<ProjectionUpdate> {
        let cursor = event
            .cursor
            .clone()
            .ok_or_else(|| ClientError::Protocol("state event omitted cursor".to_string()))?;
        if self.server_instance_id.is_empty() {
            return Err(ClientError::Protocol(
                "a snapshot must be installed before state events".to_string(),
            ));
        }
        if cursor.server_instance_id != self.server_instance_id {
            return Ok(ProjectionUpdate::RehydrationRequired(Some(Box::new(
                replay_gap("state event belongs to a different server instance"),
            ))));
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|current| cursor.sequence <= current.sequence)
        {
            return Ok(ProjectionUpdate::IgnoredStale);
        }

        let update = match event.payload.take() {
            Some(state_event::Payload::RequiredResync(resync)) => {
                ProjectionUpdate::RehydrationRequired(resync.reason.map(Box::new))
            }
            Some(state_event::Payload::Upsert(upsert)) => self.apply_upsert(&event, upsert)?,
            Some(state_event::Payload::Deleted(deleted)) => self.apply_delete(&event, deleted)?,
            None => {
                return Err(ClientError::Protocol(
                    "state event omitted its typed payload".to_string(),
                ))
            }
        };
        if !matches!(update, ProjectionUpdate::RehydrationRequired(_)) {
            self.cursor = Some(cursor);
            self.state_revision = self.state_revision.max(event.state_revision);
        }
        Ok(update)
    }

    pub fn server_instance_id(&self) -> &str {
        &self.server_instance_id
    }

    pub fn cursor(&self) -> Option<&v2::Cursor> {
        self.cursor.as_ref()
    }

    pub fn state_revision(&self) -> u64 {
        self.state_revision
    }

    pub fn sessions(&self) -> &BTreeMap<String, v2::Session> {
        &self.sessions
    }

    pub fn groups(&self) -> &BTreeMap<String, v2::Group> {
        &self.groups
    }

    pub fn processes(&self) -> &BTreeMap<String, v2::Process> {
        &self.processes
    }

    pub fn threads(&self) -> &BTreeMap<String, v2::Thread> {
        &self.threads
    }

    pub fn selections(&self) -> &BTreeMap<String, v2::Selection> {
        &self.selections
    }

    pub fn execution_states(&self) -> &BTreeMap<String, v2::ExecutionState> {
        &self.execution_states
    }

    pub fn breakpoints(&self) -> &BTreeMap<String, v2::Breakpoint> {
        &self.breakpoints
    }

    pub fn operations(&self) -> &BTreeMap<String, v2::Operation> {
        &self.operations
    }

    pub fn extension_states(&self) -> &BTreeMap<String, v2::ExtensionState> {
        &self.extension_states
    }

    pub fn pending_commands(&self) -> &BTreeMap<String, v2::PendingCommand> {
        &self.pending_commands
    }

    pub fn capabilities(&self) -> Option<&v2::Capabilities> {
        self.capabilities.as_ref()
    }

    /// Returns a detached, deterministic snapshot of the converged projection.
    ///
    /// This is intended for UI state handoff: the SDK remains responsible for
    /// revision checks, tombstones, replay, and resynchronization while the
    /// consumer receives ordinary public contract values.
    pub fn snapshot(&self) -> v2::Snapshot {
        v2::Snapshot {
            server_instance_id: self.server_instance_id.clone(),
            state_event_cursor: self.cursor.clone(),
            base_state_revision: self.state_revision,
            included_sections: self.included_sections.clone(),
            sessions: self.sessions.values().cloned().collect(),
            groups: self.groups.values().cloned().collect(),
            processes: self.processes.values().cloned().collect(),
            threads: self.threads.values().cloned().collect(),
            selection: self.selections.values().next().cloned(),
            execution_states: self.execution_states.values().cloned().collect(),
            breakpoints: self.breakpoints.values().cloned().collect(),
            pending_commands: self.pending_commands.values().cloned().collect(),
            operations: self.operations.values().cloned().collect(),
            extension_states: self.extension_states.values().cloned().collect(),
            capabilities: self.capabilities.clone(),
        }
    }

    fn apply_upsert(
        &mut self,
        event: &v2::StateEvent,
        upsert: v2::ResourceUpsert,
    ) -> Result<ProjectionUpdate> {
        let resource = upsert.resource.ok_or_else(|| {
            ClientError::Protocol("resource upsert omitted its typed resource".to_string())
        })?;
        let (kind, id, revision) = resource_metadata(&resource);
        validate_event_metadata(event, kind, &id, revision)?;
        if self
            .tombstones
            .get(&(kind as i32, id.clone()))
            .is_some_and(|deleted_revision| *deleted_revision >= revision)
        {
            return Ok(ProjectionUpdate::IgnoredStale);
        }

        let applied = match resource {
            resource_upsert::Resource::Session(value) => {
                upsert_newer(&mut self.sessions, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::Group(value) => {
                upsert_newer(&mut self.groups, id, revision, value, |item| item.revision)
            }
            resource_upsert::Resource::Process(value) => {
                upsert_newer(&mut self.processes, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::Thread(value) => {
                upsert_newer(&mut self.threads, id, revision, value, |item| item.revision)
            }
            resource_upsert::Resource::Selection(value) => {
                upsert_newer(&mut self.selections, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::ExecutionState(value) => {
                upsert_newer(&mut self.execution_states, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::Breakpoint(value) => {
                upsert_newer(&mut self.breakpoints, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::Operation(value) => {
                upsert_newer(&mut self.operations, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::Capabilities(value) => {
                if self
                    .capabilities
                    .as_ref()
                    .is_some_and(|current| current.revision >= revision)
                {
                    false
                } else {
                    self.capabilities = Some(value);
                    true
                }
            }
            resource_upsert::Resource::ExtensionState(value) => {
                upsert_newer(&mut self.extension_states, id, revision, value, |item| {
                    item.revision
                })
            }
            resource_upsert::Resource::PendingCommand(value) => {
                upsert_newer(&mut self.pending_commands, id, revision, value, |item| {
                    item.revision
                })
            }
        };
        Ok(if applied {
            ProjectionUpdate::Applied
        } else {
            ProjectionUpdate::IgnoredStale
        })
    }

    fn apply_delete(
        &mut self,
        event: &v2::StateEvent,
        deleted: v2::ResourceDeleted,
    ) -> Result<ProjectionUpdate> {
        if event.resource_kind != deleted.resource_kind
            || event.resource_id != deleted.resource_id
            || event.resource_revision != deleted.resource_revision
        {
            return Err(ClientError::Protocol(
                "state-event metadata does not match its resource tombstone".to_string(),
            ));
        }
        let key = (deleted.resource_kind, deleted.resource_id.clone());
        if self
            .tombstones
            .get(&key)
            .is_some_and(|revision| *revision >= deleted.resource_revision)
        {
            return Ok(ProjectionUpdate::IgnoredStale);
        }

        let removed = match v2::ResourceKind::try_from(deleted.resource_kind) {
            Ok(v2::ResourceKind::Session) => remove_not_newer(
                &mut self.sessions,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Group) => remove_not_newer(
                &mut self.groups,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Process) => remove_not_newer(
                &mut self.processes,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Thread) => remove_not_newer(
                &mut self.threads,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Selection) => remove_not_newer(
                &mut self.selections,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::ExecutionState) => remove_not_newer(
                &mut self.execution_states,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Breakpoint) => remove_not_newer(
                &mut self.breakpoints,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Operation) => remove_not_newer(
                &mut self.operations,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Capabilities) => {
                if self
                    .capabilities
                    .as_ref()
                    .is_some_and(|item| item.revision > deleted.resource_revision)
                {
                    false
                } else {
                    self.capabilities.take().is_some()
                }
            }
            Ok(v2::ResourceKind::ExtensionState) => remove_not_newer(
                &mut self.extension_states,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::PendingCommand) => remove_not_newer(
                &mut self.pending_commands,
                &deleted.resource_id,
                deleted.resource_revision,
                |item| item.revision,
            ),
            Ok(v2::ResourceKind::Unspecified) | Err(_) => false,
        };
        self.tombstones.insert(key, deleted.resource_revision);
        Ok(if removed {
            ProjectionUpdate::Applied
        } else {
            ProjectionUpdate::IgnoredStale
        })
    }
}

fn insert_snapshot_resources<T>(
    map: &mut BTreeMap<String, T>,
    resources: Vec<T>,
    metadata: impl Fn(&T) -> (&String, u64),
    name: &str,
) -> Result<()> {
    for resource in resources {
        let (id, revision) = metadata(&resource);
        insert_snapshot_resource(map, id.clone(), revision, resource, name)?;
    }
    Ok(())
}

fn insert_snapshot_resource<T>(
    map: &mut BTreeMap<String, T>,
    id: String,
    revision: u64,
    resource: T,
    name: &str,
) -> Result<()> {
    if id.is_empty() || revision == 0 {
        return Err(ClientError::Protocol(format!(
            "snapshot {name} has an empty identity or zero revision"
        )));
    }
    if map.insert(id, resource).is_some() {
        return Err(ClientError::Protocol(format!(
            "snapshot contains a duplicate {name} identity"
        )));
    }
    Ok(())
}

fn resource_metadata(resource: &resource_upsert::Resource) -> (v2::ResourceKind, String, u64) {
    match resource {
        resource_upsert::Resource::Session(item) => (
            v2::ResourceKind::Session,
            item.session_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Group(item) => (
            v2::ResourceKind::Group,
            item.group_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Process(item) => (
            v2::ResourceKind::Process,
            item.process_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Thread(item) => (
            v2::ResourceKind::Thread,
            item.thread_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Selection(item) => (
            v2::ResourceKind::Selection,
            item.selection_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::ExecutionState(item) => (
            v2::ResourceKind::ExecutionState,
            item.execution_state_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Breakpoint(item) => (
            v2::ResourceKind::Breakpoint,
            item.breakpoint_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Operation(item) => (
            v2::ResourceKind::Operation,
            item.operation_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::Capabilities(item) => (
            v2::ResourceKind::Capabilities,
            item.capabilities_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::ExtensionState(item) => (
            v2::ResourceKind::ExtensionState,
            item.extension_state_id.clone(),
            item.revision,
        ),
        resource_upsert::Resource::PendingCommand(item) => (
            v2::ResourceKind::PendingCommand,
            item.pending_command_id.clone(),
            item.revision,
        ),
    }
}

fn validate_event_metadata(
    event: &v2::StateEvent,
    kind: v2::ResourceKind,
    id: &str,
    revision: u64,
) -> Result<()> {
    if id.is_empty() || revision == 0 {
        return Err(ClientError::Protocol(
            "resource upsert has an empty identity or zero revision".to_string(),
        ));
    }
    if event.resource_kind != kind as i32
        || event.resource_id != id
        || event.resource_revision != revision
    {
        return Err(ClientError::Protocol(
            "state-event metadata does not match its resource upsert".to_string(),
        ));
    }
    Ok(())
}

fn upsert_newer<T>(
    map: &mut BTreeMap<String, T>,
    id: String,
    revision: u64,
    value: T,
    current_revision: impl Fn(&T) -> u64,
) -> bool {
    if map
        .get(&id)
        .is_some_and(|current| current_revision(current) >= revision)
    {
        return false;
    }
    map.insert(id, value);
    true
}

fn remove_not_newer<T>(
    map: &mut BTreeMap<String, T>,
    id: &str,
    tombstone_revision: u64,
    current_revision: impl Fn(&T) -> u64,
) -> bool {
    if map
        .get(id)
        .is_some_and(|current| current_revision(current) > tombstone_revision)
    {
        return false;
    }
    map.remove(id).is_some()
}

fn replay_gap(message: &str) -> v2::DdbError {
    v2::DdbError {
        code: v2::DdbErrorCode::ReplayGap as i32,
        message: message.to_string(),
        retryable: false,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(revision: u64) -> v2::Session {
        v2::Session {
            session_id: "ses_one".to_string(),
            display_name: format!("session-{revision}"),
            revision,
            ..Default::default()
        }
    }

    fn snapshot(resource: v2::Session) -> v2::Snapshot {
        v2::Snapshot {
            server_instance_id: "server-a".to_string(),
            state_event_cursor: Some(v2::Cursor {
                server_instance_id: "server-a".to_string(),
                sequence: 10,
            }),
            base_state_revision: 20,
            sessions: vec![resource],
            ..Default::default()
        }
    }

    fn upsert_event(sequence: u64, resource: v2::Session) -> v2::StateEvent {
        v2::StateEvent {
            cursor: Some(v2::Cursor {
                server_instance_id: "server-a".to_string(),
                sequence,
            }),
            state_revision: 20 + sequence,
            resource_kind: v2::ResourceKind::Session as i32,
            resource_id: resource.session_id.clone(),
            resource_revision: resource.revision,
            payload: Some(state_event::Payload::Upsert(v2::ResourceUpsert {
                resource: Some(resource_upsert::Resource::Session(resource)),
            })),
            ..Default::default()
        }
    }

    #[test]
    fn duplicate_and_stale_upserts_are_harmless() {
        let mut projection = StateProjection::from_snapshot(snapshot(session(3))).unwrap();
        assert_eq!(
            projection
                .apply_event(upsert_event(11, session(4)))
                .unwrap(),
            ProjectionUpdate::Applied
        );
        assert_eq!(
            projection
                .apply_event(upsert_event(11, session(4)))
                .unwrap(),
            ProjectionUpdate::IgnoredStale
        );
        assert_eq!(
            projection
                .apply_event(upsert_event(12, session(2)))
                .unwrap(),
            ProjectionUpdate::IgnoredStale
        );
        assert_eq!(projection.sessions()["ses_one"].revision, 4);
        assert_eq!(projection.cursor().unwrap().sequence, 12);
    }

    #[test]
    fn tombstone_prevents_stale_resurrection() {
        let mut projection = StateProjection::from_snapshot(snapshot(session(3))).unwrap();
        let deleted = v2::ResourceDeleted {
            resource_kind: v2::ResourceKind::Session as i32,
            resource_id: "ses_one".to_string(),
            resource_revision: 4,
        };
        let event = v2::StateEvent {
            cursor: Some(v2::Cursor {
                server_instance_id: "server-a".to_string(),
                sequence: 11,
            }),
            state_revision: 31,
            resource_kind: deleted.resource_kind,
            resource_id: deleted.resource_id.clone(),
            resource_revision: deleted.resource_revision,
            payload: Some(state_event::Payload::Deleted(deleted)),
            ..Default::default()
        };
        assert_eq!(
            projection.apply_event(event).unwrap(),
            ProjectionUpdate::Applied
        );
        assert_eq!(
            projection
                .apply_event(upsert_event(12, session(4)))
                .unwrap(),
            ProjectionUpdate::IgnoredStale
        );
        assert!(projection.sessions().is_empty());
    }

    #[test]
    fn another_server_instance_requires_rehydration() {
        let mut projection = StateProjection::from_snapshot(snapshot(session(3))).unwrap();
        let mut event = upsert_event(11, session(4));
        event.cursor.as_mut().unwrap().server_instance_id = "server-b".to_string();
        assert!(matches!(
            projection.apply_event(event).unwrap(),
            ProjectionUpdate::RehydrationRequired(Some(error))
                if error.code == v2::DdbErrorCode::ReplayGap as i32
        ));
        assert_eq!(projection.sessions()["ses_one"].revision, 3);
    }
}
