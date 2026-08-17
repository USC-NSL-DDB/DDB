use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ddb_api_types::v2::{
    resource_upsert, state_event, target, BroadcastTarget, DdbErrorCode, ExecutionState,
    GroupTarget, RequiredResync, ResourceDeleted, ResourceKind, ResourceUpsert, SessionTarget,
    StateEventKind, Target, ThreadTarget,
};
use tokio::sync::broadcast;
use tracing::warn;

use crate::{
    api::read_model::{ApiPendingCommandView, StateSnapshotView},
    cmd_flow::session_runtime::PendingCommandChange,
    state::{RuntimeChange, RuntimeResourceId},
};

use super::{
    projection::pending_command_internal_id, service::DdbApplicationService, ApplicationError,
    ResourceIdKind, StateChange, StateEventContext,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    id_kind: ResourceIdKind,
    internal_id: String,
}

#[derive(Clone, PartialEq)]
struct ProjectedResource {
    key: CacheKey,
    resource_kind: ResourceKind,
    public_id: String,
    revision: u64,
    event_kind: StateEventKind,
    operation_id: Option<String>,
    resource: resource_upsert::Resource,
    context: StateEventContext,
}

#[derive(Default)]
struct RuntimeProjectionCache {
    topology: HashMap<CacheKey, ProjectedResource>,
    breakpoints: HashMap<CacheKey, ProjectedResource>,
}

impl DdbApplicationService {
    pub(super) fn spawn_runtime_event_bridge(
        self: &Arc<Self>,
        mut changes: broadcast::Receiver<RuntimeChange>,
    ) -> tokio::task::JoinHandle<()> {
        let service = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut cache = RuntimeProjectionCache::default();
            let Some(current) = service.upgrade() else {
                return;
            };
            if let Err(error) = current.reconcile_all_runtime_state(&mut cache).await {
                current.publish_required_resync(&error);
            }
            drop(current);

            loop {
                match changes.recv().await {
                    Ok(change) => {
                        let Some(current) = service.upgrade() else {
                            return;
                        };
                        if let Err(error) = current.apply_runtime_change(&mut cache, change).await {
                            current.publish_required_resync(&error);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let Some(current) = service.upgrade() else {
                            return;
                        };
                        warn!(
                            skipped,
                            "runtime event bridge lagged; publishing a required-resync marker"
                        );
                        let error = ApplicationError::new(
                            DdbErrorCode::ReplayGap,
                            "runtime state changes outpaced the event projector",
                        )
                        .retryable(true);
                        current.publish_required_resync(&error);
                        if let Err(error) = current.reconcile_all_runtime_state(&mut cache).await {
                            current.publish_required_resync(&error);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    pub(super) fn spawn_pending_event_bridge(
        self: &Arc<Self>,
        mut changes: broadcast::Receiver<PendingCommandChange>,
    ) -> tokio::task::JoinHandle<()> {
        let service = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut cache = HashMap::new();
            let Some(current) = service.upgrade() else {
                return;
            };
            if let Err(error) = current.reconcile_pending_commands(&mut cache) {
                current.publish_required_resync(&error);
            }
            drop(current);

            loop {
                match changes.recv().await {
                    Ok(change) => {
                        let Some(current) = service.upgrade() else {
                            return;
                        };
                        if let Err(error) = current.apply_pending_change(&mut cache, change) {
                            current.publish_required_resync(&error);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let Some(current) = service.upgrade() else {
                            return;
                        };
                        warn!(
                            skipped,
                            "pending-command event bridge lagged; publishing a required-resync marker"
                        );
                        let error = ApplicationError::new(
                            DdbErrorCode::ReplayGap,
                            "pending command changes outpaced the event projector",
                        )
                        .retryable(true);
                        current.publish_required_resync(&error);
                        if let Err(error) = current.reconcile_pending_commands(&mut cache) {
                            current.publish_required_resync(&error);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }

    fn reconcile_pending_commands(
        &self,
        cache: &mut HashMap<CacheKey, ProjectedResource>,
    ) -> Result<(), ApplicationError> {
        let current = self
            .queries
            .pending_command_details()
            .into_iter()
            .map(|view| self.project_pending_command(view))
            .collect::<Result<Vec<_>, _>>()?;
        self.reconcile_resources(cache, current)
    }

    fn apply_pending_change(
        &self,
        cache: &mut HashMap<CacheKey, ProjectedResource>,
        change: PendingCommandChange,
    ) -> Result<(), ApplicationError> {
        match change {
            PendingCommandChange::Upsert(command) => {
                let resource = self.project_pending_command(command.into())?;
                self.publish_projected_resource(cache, resource)
            }
            PendingCommandChange::Removed { sid, token } => {
                let internal_id = pending_command_internal_id(sid, token);
                let key = CacheKey {
                    id_kind: ResourceIdKind::PendingCommand,
                    internal_id,
                };
                let cached = cache.remove(&key);
                let previous_revision = cached.as_ref().map(|resource| resource.revision);
                let operation_id = cached
                    .as_ref()
                    .and_then(|resource| resource.operation_id.clone());
                let context = match cached {
                    Some(resource) => resource.context,
                    None => {
                        let mut context = StateEventContext::default();
                        context.add_session(self.ids.encode(ResourceIdKind::Session, sid)?);
                        context
                    }
                };
                self.publish_tombstone(
                    &key,
                    ResourceKind::PendingCommand,
                    previous_revision,
                    operation_id,
                    context,
                )
            }
            PendingCommandChange::Reconcile => self.reconcile_pending_commands(cache),
        }
    }

    fn project_pending_command(
        &self,
        view: ApiPendingCommandView,
    ) -> Result<ProjectedResource, ApplicationError> {
        let internal_id = pending_command_internal_id(view.sid, view.token);
        let operation_id = view.operation_id.clone();
        let resource = self.projection().pending_command(&view)?;
        let mut resource = projected(
            ResourceIdKind::PendingCommand,
            internal_id,
            ResourceKind::PendingCommand,
            resource.pending_command_id.clone(),
            resource.revision,
            StateEventKind::ResourceUpserted,
            resource_upsert::Resource::PendingCommand(resource),
        );
        resource.operation_id = operation_id;
        Ok(resource)
    }

    async fn reconcile_all_runtime_state(
        &self,
        cache: &mut RuntimeProjectionCache,
    ) -> Result<(), ApplicationError> {
        let topology = self.project_topology().await?;
        self.reconcile_resources(&mut cache.topology, topology)?;
        let breakpoints = self.project_breakpoints()?;
        self.reconcile_resources(&mut cache.breakpoints, breakpoints)
    }

    async fn apply_runtime_change(
        &self,
        cache: &mut RuntimeProjectionCache,
        change: RuntimeChange,
    ) -> Result<(), ApplicationError> {
        let mut removed = HashSet::new();
        for resource in change.removed {
            if removed.insert(resource) {
                self.publish_explicit_tombstone(cache, resource)?;
            }
        }
        if change.topology {
            let topology = self.project_topology().await?;
            self.reconcile_resources(&mut cache.topology, topology)?;
        }
        if change.breakpoints {
            let breakpoints = self.project_breakpoints()?;
            self.reconcile_resources(&mut cache.breakpoints, breakpoints)?;
        }
        Ok(())
    }

    async fn project_topology(&self) -> Result<Vec<ProjectedResource>, ApplicationError> {
        let snapshot = self.queries.snapshot().await;
        let projection = self.projection();
        let mut resources = Vec::with_capacity(
            snapshot.sessions.len()
                + snapshot.groups.len()
                + snapshot.processes.len()
                + snapshot.threads.len()
                + snapshot.extensions.len()
                + 2,
        );

        for view in &snapshot.sessions {
            let resource = projection.session(view)?;
            resources.push(projected(
                ResourceIdKind::Session,
                view.sid,
                ResourceKind::Session,
                resource.session_id.clone(),
                resource.revision,
                StateEventKind::ResourceUpserted,
                resource_upsert::Resource::Session(resource),
            ));
        }
        for view in &snapshot.groups {
            let resource = projection.group(view, snapshot.selected_session_id)?;
            resources.push(projected(
                ResourceIdKind::Group,
                view.id,
                ResourceKind::Group,
                resource.group_id.clone(),
                resource.revision,
                StateEventKind::ResourceUpserted,
                resource_upsert::Resource::Group(resource),
            ));
        }
        for view in &snapshot.processes {
            let resource = projection.process(view)?;
            resources.push(projected(
                ResourceIdKind::Process,
                view.global_id,
                ResourceKind::Process,
                resource.process_id.clone(),
                resource.revision,
                StateEventKind::ResourceUpserted,
                resource_upsert::Resource::Process(resource),
            ));
        }
        for view in &snapshot.threads {
            let resource = projection.thread(view)?;
            resources.push(projected(
                ResourceIdKind::Thread,
                view.global_id,
                ResourceKind::Thread,
                resource.thread_id.clone(),
                resource.revision,
                StateEventKind::ResourceUpserted,
                resource_upsert::Resource::Thread(resource),
            ));
        }
        for (target_key, resource) in self.project_execution_state_entries(&snapshot)? {
            let mut projected = projected(
                ResourceIdKind::ExecutionState,
                target_key,
                ResourceKind::ExecutionState,
                resource.execution_state_id.clone(),
                resource.revision,
                StateEventKind::ExecutionChanged,
                resource_upsert::Resource::ExecutionState(resource.clone()),
            );
            self.add_execution_owner_context(&mut projected.context, &resource, &snapshot)?;
            resources.push(projected);
        }

        let selection = self.selection(
            snapshot.selected_session_id,
            snapshot.selected_thread_id,
            &snapshot.groups,
        )?;
        resources.push(projected(
            ResourceIdKind::Selection,
            "current",
            ResourceKind::Selection,
            selection.selection_id.clone(),
            selection.revision,
            StateEventKind::SelectionChanged,
            resource_upsert::Resource::Selection(selection),
        ));

        let capabilities = self.capabilities()?;
        resources.push(projected(
            ResourceIdKind::Capabilities,
            "current",
            ResourceKind::Capabilities,
            capabilities.capabilities_id.clone(),
            capabilities.revision,
            StateEventKind::CapabilitiesChanged,
            resource_upsert::Resource::Capabilities(capabilities),
        ));

        for state in &snapshot.extensions {
            let resource = projection.extension_state(state)?;
            resources.push(projected(
                ResourceIdKind::Extension,
                &state.extension_id,
                ResourceKind::ExtensionState,
                resource.extension_state_id.clone(),
                resource.revision,
                StateEventKind::ExtensionStateChanged,
                resource_upsert::Resource::ExtensionState(resource),
            ));
        }
        Ok(resources)
    }

    pub(super) fn project_execution_states(
        &self,
        snapshot: &StateSnapshotView,
    ) -> Result<Vec<ExecutionState>, ApplicationError> {
        Ok(self
            .project_execution_state_entries(snapshot)?
            .into_iter()
            .map(|(_, state)| state)
            .collect())
    }

    fn project_execution_state_entries(
        &self,
        snapshot: &StateSnapshotView,
    ) -> Result<Vec<(String, ExecutionState)>, ApplicationError> {
        let projection = self.projection();
        let mut states = Vec::new();

        for thread in &snapshot.threads {
            let target = Target {
                selector: Some(target::Selector::Thread(ThreadTarget {
                    thread_id: self.ids.encode(ResourceIdKind::Thread, thread.global_id)?,
                })),
            };
            if let Some(state) = projection.execution_state(target, std::slice::from_ref(thread))? {
                states.push(state);
            }
        }

        for session in &snapshot.sessions {
            let threads = snapshot
                .threads
                .iter()
                .filter(|thread| thread.session_id == session.sid)
                .cloned()
                .collect::<Vec<_>>();
            let target = Target {
                selector: Some(target::Selector::Session(SessionTarget {
                    session_id: self.ids.encode(ResourceIdKind::Session, session.sid)?,
                })),
            };
            if let Some(state) = projection.execution_state(target, &threads)? {
                states.push(state);
            }
        }

        for group in &snapshot.groups {
            let threads = snapshot
                .threads
                .iter()
                .filter(|thread| thread.group_id == Some(group.id))
                .cloned()
                .collect::<Vec<_>>();
            let target = Target {
                selector: Some(target::Selector::Group(GroupTarget {
                    group_id: self.ids.encode(ResourceIdKind::Group, group.id)?,
                })),
            };
            if let Some(state) = projection.execution_state(target, &threads)? {
                states.push(state);
            }
        }

        let target = Target {
            selector: Some(target::Selector::Broadcast(BroadcastTarget {})),
        };
        if let Some(state) = projection.execution_state(target, &snapshot.threads)? {
            states.push(state);
        }
        states.sort_unstable_by(|left, right| {
            left.1.execution_state_id.cmp(&right.1.execution_state_id)
        });
        Ok(states)
    }

    fn add_execution_owner_context(
        &self,
        context: &mut StateEventContext,
        resource: &ExecutionState,
        snapshot: &StateSnapshotView,
    ) -> Result<(), ApplicationError> {
        let Some(Target {
            selector: Some(target::Selector::Thread(target)),
        }) = resource.target.as_ref()
        else {
            return Ok(());
        };
        let internal_id = self
            .ids
            .decode(ResourceIdKind::Thread, &target.thread_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::backend("invalid internal thread identity"))?;
        let thread = snapshot
            .threads
            .iter()
            .find(|thread| thread.global_id == internal_id)
            .ok_or_else(|| {
                ApplicationError::backend("execution-state thread is not in topology")
            })?;
        context.add_session(
            self.ids
                .encode(ResourceIdKind::Session, thread.session_id)?,
        );
        if let Some(group_id) = thread.group_id {
            context.add_group(self.ids.encode(ResourceIdKind::Group, group_id)?);
        }
        Ok(())
    }

    fn project_breakpoints(&self) -> Result<Vec<ProjectedResource>, ApplicationError> {
        let projection = self.projection();
        self.queries
            .breakpoints()
            .iter()
            .map(|snapshot| {
                let resource = projection.breakpoint(snapshot)?;
                Ok(projected(
                    ResourceIdKind::Breakpoint,
                    snapshot.id,
                    ResourceKind::Breakpoint,
                    resource.breakpoint_id.clone(),
                    resource.revision,
                    StateEventKind::ResourceUpserted,
                    resource_upsert::Resource::Breakpoint(resource),
                ))
            })
            .collect()
    }

    fn reconcile_resources(
        &self,
        cache: &mut HashMap<CacheKey, ProjectedResource>,
        current: Vec<ProjectedResource>,
    ) -> Result<(), ApplicationError> {
        let current = current
            .into_iter()
            .map(|resource| (resource.key.clone(), resource))
            .collect::<HashMap<_, _>>();

        let mut deleted = cache
            .keys()
            .filter(|key| !current.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        deleted.sort_unstable_by(|left, right| cache[left].public_id.cmp(&cache[right].public_id));
        for key in deleted {
            let previous = cache
                .remove(&key)
                .expect("deleted resource must exist in the projection cache");
            self.publish_tombstone(
                &previous.key,
                previous.resource_kind,
                Some(previous.revision),
                previous.operation_id,
                previous.context,
            )?;
        }

        let mut ordered = current.into_values().collect::<Vec<_>>();
        ordered.sort_unstable_by(|left, right| {
            (left.resource_kind as i32, &left.public_id)
                .cmp(&(right.resource_kind as i32, &right.public_id))
        });
        for resource in ordered {
            self.publish_projected_resource(cache, resource)?;
        }
        Ok(())
    }

    fn publish_projected_resource(
        &self,
        cache: &mut HashMap<CacheKey, ProjectedResource>,
        resource: ProjectedResource,
    ) -> Result<(), ApplicationError> {
        if cache
            .get(&resource.key)
            .is_some_and(|previous| previous == &resource)
        {
            return Ok(());
        }
        let mut event_context = resource.context.clone();
        if let Some(previous) = cache.get(&resource.key) {
            event_context.merge(&previous.context);
        }
        self.journal.publish(StateChange {
            request_id: None,
            operation_id: resource.operation_id.clone(),
            kind: resource.event_kind,
            resource_kind: resource.resource_kind,
            resource_id: resource.public_id.clone(),
            resource_revision: resource.revision,
            payload: state_event::Payload::Upsert(ResourceUpsert {
                resource: Some(resource.resource.clone()),
            }),
            extension_details: Vec::new(),
            context: event_context,
        })?;
        cache.insert(resource.key.clone(), resource);
        Ok(())
    }

    fn publish_explicit_tombstone(
        &self,
        cache: &mut RuntimeProjectionCache,
        removed: RuntimeResourceId,
    ) -> Result<(), ApplicationError> {
        let (key, resource_kind) = runtime_resource_key(removed);
        let cached = cache
            .topology
            .remove(&key)
            .or_else(|| cache.breakpoints.remove(&key));
        let previous_revision = cached.as_ref().map(|resource| resource.revision);
        let operation_id = cached
            .as_ref()
            .and_then(|resource| resource.operation_id.clone());
        let context = match cached {
            Some(resource) => resource.context,
            None => {
                let mut context = StateEventContext::default();
                match resource_kind {
                    ResourceKind::Session => context
                        .add_session(self.ids.encode(ResourceIdKind::Session, &key.internal_id)?),
                    ResourceKind::Group => {
                        context.add_group(self.ids.encode(ResourceIdKind::Group, &key.internal_id)?)
                    }
                    _ => context.mark_global(),
                }
                context
            }
        };
        self.publish_tombstone(
            &key,
            resource_kind,
            previous_revision,
            operation_id,
            context,
        )
    }

    fn publish_tombstone(
        &self,
        key: &CacheKey,
        resource_kind: ResourceKind,
        previous_revision: Option<u64>,
        operation_id: Option<String>,
        context: StateEventContext,
    ) -> Result<(), ApplicationError> {
        let public_id = self.ids.encode(key.id_kind, &key.internal_id)?;
        let observed = self.resources.observe(key.id_kind, &key.internal_id)?;
        let revision = if previous_revision.is_some_and(|revision| observed.revision > revision) {
            observed.revision
        } else {
            self.resources.bump(key.id_kind, &key.internal_id)?.revision
        };
        self.journal.publish(StateChange {
            request_id: None,
            operation_id,
            kind: StateEventKind::ResourceDeleted,
            resource_kind,
            resource_id: public_id.clone(),
            resource_revision: revision,
            payload: state_event::Payload::Deleted(ResourceDeleted {
                resource_kind: resource_kind as i32,
                resource_id: public_id,
                resource_revision: revision,
            }),
            extension_details: Vec::new(),
            context,
        })?;
        Ok(())
    }

    fn publish_required_resync(&self, cause: &ApplicationError) {
        let request_id = format!("runtime-bridge-{}", self.server_instance_id());
        let result = self.journal.publish(StateChange {
            request_id: None,
            operation_id: None,
            kind: StateEventKind::RequiredResync,
            resource_kind: ResourceKind::Unspecified,
            resource_id: String::new(),
            resource_revision: 0,
            payload: state_event::Payload::RequiredResync(RequiredResync {
                reason: Some(cause.to_contract(request_id)),
            }),
            extension_details: Vec::new(),
            context: StateEventContext::default(),
        });
        if let Err(error) = result {
            warn!(
                code = ?error.code(),
                "required-resync event could not be published"
            );
        }
    }
}

fn projected(
    id_kind: ResourceIdKind,
    internal_id: impl ToString,
    resource_kind: ResourceKind,
    public_id: String,
    revision: u64,
    event_kind: StateEventKind,
    resource: resource_upsert::Resource,
) -> ProjectedResource {
    let context = StateEventContext::from_resource(&resource);
    ProjectedResource {
        key: CacheKey {
            id_kind,
            internal_id: internal_id.to_string(),
        },
        resource_kind,
        public_id,
        revision,
        event_kind,
        operation_id: None,
        resource,
        context,
    }
}

fn runtime_resource_key(resource: RuntimeResourceId) -> (CacheKey, ResourceKind) {
    let (id_kind, internal_id, resource_kind) = match resource {
        RuntimeResourceId::Session(id) => (ResourceIdKind::Session, id, ResourceKind::Session),
        RuntimeResourceId::Group(id) => (ResourceIdKind::Group, id, ResourceKind::Group),
        RuntimeResourceId::Process(id) => (ResourceIdKind::Process, id, ResourceKind::Process),
        RuntimeResourceId::Thread(id) => (ResourceIdKind::Thread, id, ResourceKind::Thread),
        RuntimeResourceId::Breakpoint(id) => {
            (ResourceIdKind::Breakpoint, id, ResourceKind::Breakpoint)
        }
    };
    (
        CacheKey {
            id_kind,
            internal_id: internal_id.to_string(),
        },
        resource_kind,
    )
}
