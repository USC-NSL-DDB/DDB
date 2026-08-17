use std::sync::Arc;

use tokio::sync::{broadcast, OwnedMutexGuard};

use super::{
    bkpt_mgr::{BkptLoc, BkptMeta, BreakpointMgr, BreakpointStateChange, SubBkptMeta, SubBkptSpec},
    bkpt_snapshot::BreakpointSnapshot,
    group_mgr::{GroupMeta, GroupMgr},
    group_operation::GroupOperationCoordinator,
    ids::{GlobalThreadGroupId, GlobalThreadId, GroupId, ServiceIdentity},
    proclet_mgr::ProcletMgr,
    session_mgr::{SessionMeta, SessionStatus, ThreadContext, ThreadLocation, ThreadStatus},
    state_mgr::{GlobalThreadIdentity, StateMgr, StateTransitionResult},
    thread_mgr::{LocalThreadId, ThreadIdView},
};

#[cfg(test)]
use super::bkpt_mgr::SubBkptType;

const RUNTIME_CHANGE_QUEUE: usize = 4_096;

/// Backend-neutral hints emitted only after client-visible runtime state has
/// committed. Consumers must resample detached state; the domain layer never
/// owns public API payloads, identities, or revisions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RuntimeResourceId {
    Session(u64),
    Group(u64),
    Process(u64),
    Thread(u64),
    Breakpoint(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeChange {
    pub(crate) topology: bool,
    pub(crate) breakpoints: bool,
    pub(crate) removed: Vec<RuntimeResourceId>,
}

impl RuntimeChange {
    fn topology() -> Self {
        Self {
            topology: true,
            breakpoints: false,
            removed: Vec::new(),
        }
    }

    fn breakpoints() -> Self {
        Self {
            topology: false,
            breakpoints: true,
            removed: Vec::new(),
        }
    }
}

/// Immutable session state returned across the runtime-model boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSnapshot {
    pub(crate) sid: u64,
    pub(crate) tag: String,
    pub(crate) service_identity: Option<ServiceIdentity>,
    pub(crate) status: SessionStatus,
    pub(crate) current_context: Option<ThreadContext>,
    pub(crate) in_custom_context: bool,
    pub(crate) all_threads_stopped: bool,
}

impl SessionSnapshot {
    fn from_meta(meta: &SessionMeta) -> Self {
        Self {
            sid: meta.sid(),
            tag: meta.tag().to_string(),
            service_identity: meta.cloned_service_identity(),
            status: meta.status(),
            current_context: meta.current_context().cloned(),
            in_custom_context: meta.is_in_custom_context(),
            all_threads_stopped: meta.all_threads_stopped(),
        }
    }
}

/// Immutable thread state returned across the runtime-model boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ThreadSnapshot {
    pub(crate) global_id: GlobalThreadId,
    pub(crate) process_id: Option<GlobalThreadGroupId>,
    pub(crate) session_id: u64,
    pub(crate) local_id: u64,
    pub(crate) status: ThreadStatus,
    pub(crate) selected: bool,
    pub(crate) execution_revision: u64,
    pub(crate) location: Option<ThreadLocation>,
}

/// Immutable process state returned across the runtime-model boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessSnapshot {
    pub(crate) global_id: GlobalThreadGroupId,
    pub(crate) session_id: u64,
    pub(crate) system_process_id: Option<u64>,
}

/// Holds a group operation gate across debugger I/O without exposing the
/// coordinator or its mutable gate registry.
pub(crate) struct GroupOperationGuard {
    _guard: OwnedMutexGuard<()>,
}

/// Holds all requested group gates in canonical order.
pub(crate) struct GroupOperationSet {
    _guards: Vec<OwnedMutexGuard<()>>,
}

/// A retirement token proves that the session's group gate is held. The only
/// way to retire a session is to obtain this token and consume it.
#[must_use = "dropping a pending retirement releases the group gate without retiring the session"]
pub(crate) struct PendingSessionRetirement {
    model: Arc<RuntimeModel>,
    sid: u64,
    group_operation: Option<GroupOperationGuard>,
}

impl PendingSessionRetirement {
    pub(crate) async fn finish(self) -> SessionRetirement {
        let Self {
            model,
            sid,
            group_operation,
        } = self;
        let (breakpoint_changes, emptied_group) = model.retire_session_locked(sid).await;
        SessionRetirement {
            model,
            group_operation,
            breakpoint_changes,
            emptied_group,
        }
    }
}

/// Owns every mutable debugger-domain repository and exposes only coordinated
/// commands, immutable snapshots, and short read-only identity views.
///
/// Internal repositories retain their specialized synchronization. Callers can
/// no longer acquire repository handles or session write guards, so cross-store
/// ordering is enforced here instead of by convention.
///
/// Lock hierarchy:
/// group operation gate (tokio) -> session meta (tokio) -> repository leaf
/// locks (std). Leaf locks are never held across an await.
pub struct RuntimeModel {
    state: StateMgr,
    groups: GroupMgr,
    breakpoints: BreakpointMgr,
    proclets: ProcletMgr,
    group_operations: GroupOperationCoordinator,
    changes: broadcast::Sender<RuntimeChange>,
}

impl std::fmt::Debug for RuntimeModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeModel")
            .finish_non_exhaustive()
    }
}

impl RuntimeModel {
    pub fn new() -> Arc<Self> {
        let (changes, _) = broadcast::channel(RUNTIME_CHANGE_QUEUE);
        Arc::new(Self {
            state: StateMgr::new(),
            groups: GroupMgr::new(),
            breakpoints: BreakpointMgr::new(),
            proclets: ProcletMgr::new(),
            group_operations: GroupOperationCoordinator::new(),
            changes,
        })
    }

    pub(crate) fn subscribe_changes(&self) -> broadcast::Receiver<RuntimeChange> {
        self.changes.subscribe()
    }

    fn notify_change(&self, change: RuntimeChange) {
        // No receiver is a normal startup/shutdown state. Once subscribed, a
        // lagging consumer is explicitly detectable through broadcast::RecvError.
        let _ = self.changes.send(change);
    }

    // Session lifecycle and snapshots.

    pub(crate) async fn register_session(
        &self,
        sid: u64,
        tag: &str,
        service_identity: Option<ServiceIdentity>,
    ) {
        self.state
            .register_session(sid, tag, service_identity)
            .await;
        self.notify_change(RuntimeChange::topology());
    }

    /// Reserves the service group and publishes the session's membership only
    /// while holding the group's operation gate.
    ///
    /// The reservation is revalidated after gate acquisition: retirement may
    /// remove the reserved group while activation waits, and group ids are
    /// never reused, so a recreated group fails the id comparison and the
    /// reservation is retried. Cancellation between reservation and
    /// publication can leave an empty reserved group behind; it is adopted by
    /// the next same-hash activation and is otherwise inert (see
    /// docs/runtime-architecture.md).
    pub(crate) async fn register_service_group(
        &self,
        sid: u64,
        identity: &ServiceIdentity,
    ) -> GroupOperationGuard {
        loop {
            let group_id = self
                .groups
                .ensure_group(&identity.hash, identity.alias.clone());
            let operation = self.lock_group_operation(group_id).await;
            if self
                .groups
                .ensure_group(&identity.hash, identity.alias.clone())
                == group_id
            {
                self.groups
                    .register_session(&identity.hash, identity.alias.clone(), sid);
                self.notify_change(RuntimeChange::topology());
                return operation;
            }
        }
    }

    pub(crate) async fn complete_session_activation(&self, sid: u64, proclet_owner: Option<u32>) {
        if let Some(caladan_ip) = proclet_owner {
            self.proclets.register_owner_session(caladan_ip, sid);
        }
        self.state.update_session_status_on(sid).await;
        self.notify_change(RuntimeChange::topology());
    }

    pub(crate) async fn session_snapshot(&self, sid: u64) -> Option<SessionSnapshot> {
        let session = self.state.session(sid)?;
        Some(session.read_with(SessionSnapshot::from_meta).await)
    }

    pub(crate) async fn session_snapshots(&self) -> Vec<SessionSnapshot> {
        let mut snapshots = Vec::new();
        for session in self.state.sessions() {
            snapshots.push(session.read_with(SessionSnapshot::from_meta).await);
        }
        snapshots
    }

    pub(crate) fn session_ids(&self) -> Vec<u64> {
        self.state.session_ids()
    }

    pub(crate) async fn thread_snapshots_for_sessions(
        &self,
        session_ids: &[u64],
    ) -> Vec<ThreadSnapshot> {
        let mut snapshots = Vec::new();
        for session_id in session_ids {
            let Some(session) = self.state.session(*session_id) else {
                continue;
            };
            let (selected, threads) = session
                .read_with(|session| {
                    let (selected, threads) = session.thread_status_snapshot();
                    let threads = threads
                        .into_iter()
                        .map(|thread| {
                            let local_group_id = session
                                .thread_group_for(thread.local_id)
                                .map(str::to_string);
                            (thread, local_group_id)
                        })
                        .collect::<Vec<_>>();
                    (selected, threads)
                })
                .await;
            for (thread, local_group_id) in threads {
                if let Some(global_id) = self.state.global_thread_id(*session_id, thread.local_id) {
                    snapshots.push(ThreadSnapshot {
                        global_id,
                        process_id: local_group_id.as_deref().and_then(|local_group_id| {
                            self.state
                                .global_thread_group_id(*session_id, local_group_id)
                        }),
                        session_id: *session_id,
                        local_id: thread.local_id,
                        status: thread.status,
                        selected: selected == Some(thread.local_id),
                        execution_revision: thread.execution_revision,
                        location: thread.location,
                    });
                }
            }
        }
        snapshots.sort_unstable_by_key(|thread| thread.global_id.value());
        snapshots
    }

    pub(crate) async fn process_snapshots_for_sessions(
        &self,
        session_ids: &[u64],
    ) -> Vec<ProcessSnapshot> {
        let mut snapshots = Vec::new();
        for session_id in session_ids {
            let Some(session) = self.state.session(*session_id) else {
                continue;
            };
            let groups = session.read_with(SessionMeta::thread_group_snapshot).await;
            for (local_group_id, system_process_id) in groups {
                if let Some(global_id) = self
                    .state
                    .global_thread_group_id(*session_id, &local_group_id)
                {
                    snapshots.push(ProcessSnapshot {
                        global_id,
                        session_id: *session_id,
                        system_process_id,
                    });
                }
            }
        }
        snapshots.sort_unstable_by_key(|process| process.global_id.value());
        snapshots
    }

    #[cfg(test)]
    pub(crate) async fn session_thread_group(
        &self,
        sid: u64,
        local_thread_id: u64,
    ) -> Option<Option<String>> {
        self.state
            .with_session(sid, |session| {
                session
                    .thread_group_for(local_thread_id)
                    .map(str::to_string)
            })
            .await
    }

    pub(crate) async fn session_id_by_tag(&self, tag: &str) -> Option<u64> {
        let session = self.state.session_by_tag(tag)?;
        Some(session.read_with(|meta| meta.sid()).await)
    }

    pub(crate) async fn session_service_identity(&self, sid: u64) -> Option<ServiceIdentity> {
        self.state.session_service_identity(sid).await
    }

    pub(crate) async fn begin_session_retirement(
        self: &Arc<Self>,
        sid: u64,
    ) -> PendingSessionRetirement {
        let group_operation = match self.groups.group_id_by_session(sid) {
            Some(group_id) => Some(self.lock_group_operation(group_id).await),
            None => None,
        };
        PendingSessionRetirement {
            model: Arc::clone(self),
            sid,
            group_operation,
        }
    }

    async fn retire_session_locked(
        &self,
        sid: u64,
    ) -> (Vec<BreakpointStateChange>, Option<GroupId>) {
        let group_id = self.groups.group_id_by_session(sid);
        let removed_threads = self.global_thread_ids_for_session(sid);
        let removed_processes = self.process_snapshots_for_sessions(&[sid]).await;
        self.state.update_session_status_off(sid).await;
        let breakpoint_changes = self
            .breakpoints
            .clean_bkpts_for_terminated_session(sid, group_id);
        self.groups.remove_session(sid);
        let emptied_group =
            group_id.filter(|group_id| self.groups.group_by_id(*group_id).is_none());
        self.proclets.remove_owner_session(sid);
        self.state.remove_session(sid).await;
        let mut removed = vec![RuntimeResourceId::Session(sid)];
        removed.extend(
            removed_threads
                .into_iter()
                .map(|thread| RuntimeResourceId::Thread(thread.value())),
        );
        removed.extend(
            removed_processes
                .into_iter()
                .map(|process| RuntimeResourceId::Process(process.global_id.value())),
        );
        if let Some(group_id) = emptied_group {
            removed.push(RuntimeResourceId::Group(group_id.value()));
        }
        removed.extend(breakpoint_changes.iter().filter_map(|change| match change {
            BreakpointStateChange::Removed(id) => Some(RuntimeResourceId::Breakpoint(*id)),
            _ => None,
        }));
        self.notify_change(RuntimeChange {
            topology: true,
            breakpoints: !breakpoint_changes.is_empty(),
            removed,
        });
        (breakpoint_changes, emptied_group)
    }

    // Session operation state.

    pub(crate) async fn enter_custom_context(&self, sid: u64, context: ThreadContext) -> bool {
        let changed = self
            .state
            .with_session_mut(sid, |session| session.enter_custom_context(context))
            .await
            .is_some();
        if changed {
            self.notify_change(RuntimeChange::topology());
        }
        changed
    }

    pub(crate) async fn finish_context_restore(&self, sid: u64, restored: bool) -> bool {
        let changed = self
            .state
            .with_session_mut(sid, |session| session.exit_custom_context(restored))
            .await
            .is_some();
        if changed {
            self.notify_change(RuntimeChange::topology());
        }
        changed
    }

    pub(crate) async fn all_threads_stopped(&self, sid: u64) -> Option<bool> {
        self.state
            .with_session(sid, |session| session.all_threads_stopped())
            .await
    }

    pub(crate) async fn mark_all_threads(
        &self,
        sid: u64,
        status: ThreadStatus,
    ) -> StateTransitionResult<()> {
        let result = self.state.update_all_thread_status(sid, status).await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    // Thread topology and selection.

    pub(crate) async fn register_thread_group(
        &self,
        sid: u64,
        local_group_id: &str,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let result = self.state.register_thread_group(sid, local_group_id).await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    pub(crate) async fn remove_thread_group(
        &self,
        sid: u64,
        local_group_id: &str,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let process_id = self.state.global_thread_group_id(sid, local_group_id);
        let removed_threads = self
            .thread_snapshots_for_sessions(&[sid])
            .await
            .into_iter()
            .filter(|thread| thread.process_id == process_id)
            .map(|thread| thread.global_id)
            .collect::<Vec<_>>();
        let result = self.state.remove_thread_group(sid, local_group_id).await;
        if let Ok(process_id) = &result {
            let mut removed = vec![RuntimeResourceId::Process(process_id.value())];
            removed.extend(
                removed_threads
                    .into_iter()
                    .map(|thread| RuntimeResourceId::Thread(thread.value())),
            );
            self.notify_change(RuntimeChange {
                topology: true,
                breakpoints: false,
                removed,
            });
        }
        result
    }

    pub(crate) async fn start_thread_group(
        &self,
        sid: u64,
        local_group_id: &str,
        pid: u64,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let result = self
            .state
            .start_thread_group(sid, local_group_id, pid)
            .await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    pub(crate) async fn exit_thread_group(
        &self,
        sid: u64,
        local_group_id: &str,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let process_id = self.state.global_thread_group_id(sid, local_group_id);
        let removed_threads = self
            .thread_snapshots_for_sessions(&[sid])
            .await
            .into_iter()
            .filter(|thread| thread.process_id == process_id)
            .map(|thread| thread.global_id)
            .collect::<Vec<_>>();
        let result = self.state.exit_thread_group(sid, local_group_id).await;
        if result.is_ok() {
            self.notify_change(RuntimeChange {
                topology: true,
                breakpoints: false,
                removed: removed_threads
                    .into_iter()
                    .map(|thread| RuntimeResourceId::Thread(thread.value()))
                    .collect(),
            });
        }
        result
    }

    pub(crate) async fn register_thread(
        &self,
        sid: u64,
        local_thread_id: u64,
        local_group_id: &str,
    ) -> StateTransitionResult<GlobalThreadIdentity> {
        let result = self
            .state
            .register_thread(sid, local_thread_id, local_group_id)
            .await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    pub(crate) async fn remove_thread(
        &self,
        sid: u64,
        local_thread_id: u64,
        local_group_id: &str,
    ) -> StateTransitionResult<GlobalThreadIdentity> {
        let result = self
            .state
            .remove_thread(sid, local_thread_id, local_group_id)
            .await;
        if let Ok(identity) = &result {
            self.notify_change(RuntimeChange {
                topology: true,
                breakpoints: false,
                removed: vec![RuntimeResourceId::Thread(identity.thread_id.value())],
            });
        }
        result
    }

    #[cfg(test)]
    pub(crate) async fn update_thread_statuses(
        &self,
        sid: u64,
        local_thread_ids: &[u64],
        status: ThreadStatus,
    ) -> StateTransitionResult<()> {
        let result = self
            .state
            .update_thread_statuses(sid, local_thread_ids, status)
            .await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    pub(crate) async fn update_thread_statuses_with_location(
        &self,
        sid: u64,
        local_thread_ids: &[u64],
        status: ThreadStatus,
        location: Option<(u64, ThreadLocation)>,
    ) -> StateTransitionResult<()> {
        let result = self
            .state
            .update_thread_statuses_with_location(sid, local_thread_ids, status, location)
            .await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    pub(crate) async fn select_local_thread(
        &self,
        sid: u64,
        local_thread_id: u64,
    ) -> StateTransitionResult<()> {
        let result = self.state.select_local_thread(sid, local_thread_id).await;
        if result.is_ok() {
            self.notify_change(RuntimeChange::topology());
        }
        result
    }

    pub(crate) fn current_thread_id(&self) -> Option<GlobalThreadId> {
        self.state.current_thread_id()
    }

    pub(crate) fn current_session_id(&self) -> Option<u64> {
        self.state.current_session_id()
    }

    pub(crate) fn local_thread_id(&self, global_id: GlobalThreadId) -> Option<LocalThreadId> {
        self.state.local_thread_id(global_id)
    }

    pub(crate) fn global_thread_id(
        &self,
        sid: u64,
        local_thread_id: u64,
    ) -> Option<GlobalThreadId> {
        self.state.global_thread_id(sid, local_thread_id)
    }

    #[cfg(test)]
    pub(crate) fn global_thread_group_id(
        &self,
        sid: u64,
        local_group_id: &str,
    ) -> Option<GlobalThreadGroupId> {
        self.state.global_thread_group_id(sid, local_group_id)
    }

    pub(crate) fn global_thread_ids_for_session(&self, sid: u64) -> Vec<GlobalThreadId> {
        self.state.global_thread_ids_for_session(sid)
    }

    pub(crate) fn read_thread_ids(&self) -> ThreadIdView<'_> {
        self.state.read_thread_ids()
    }

    #[cfg(test)]
    pub(crate) fn select_thread_context(&self, sid: u64, global_id: GlobalThreadId) {
        self.state.select_thread_context(sid, global_id);
        self.notify_change(RuntimeChange::topology());
    }

    // Group queries and operation serialization.

    pub(crate) fn group_info_by_session(&self, sid: u64) -> Option<(GroupId, String)> {
        self.groups.group_info_by_session(sid)
    }

    pub(crate) fn group_hash_by_session(&self, sid: u64) -> Option<String> {
        self.groups.group_hash_by_session(sid)
    }

    pub(crate) fn group_id_by_session(&self, sid: u64) -> Option<GroupId> {
        self.groups.group_id_by_session(sid)
    }

    pub(crate) fn group_by_id(&self, group_id: GroupId) -> Option<GroupMeta> {
        self.groups.group_by_id(group_id)
    }

    pub(crate) fn group_by_hash(&self, hash: &str) -> Option<GroupMeta> {
        self.groups.group_by_hash(hash)
    }

    pub(crate) fn groups(&self) -> Vec<GroupMeta> {
        self.groups.groups()
    }

    pub(crate) fn matching_groups(&self, predicate: &dyn Fn(&GroupMeta) -> bool) -> Vec<GroupMeta> {
        self.groups.matching_groups(predicate)
    }

    pub(crate) async fn lock_group_operation(&self, group_id: GroupId) -> GroupOperationGuard {
        GroupOperationGuard {
            _guard: self.group_operations.lock(group_id).await,
        }
    }

    pub(crate) async fn lock_group_operations(
        &self,
        group_ids: impl IntoIterator<Item = GroupId>,
    ) -> GroupOperationSet {
        GroupOperationSet {
            _guards: self.group_operations.lock_many(group_ids).await,
        }
    }

    // Breakpoint commands and snapshots.

    pub(crate) fn breakpoint(&self, breakpoint_id: u64) -> Option<BkptMeta> {
        self.breakpoints.breakpoint(breakpoint_id)
    }

    pub(crate) fn breakpoint_snapshots(&self) -> Vec<BreakpointSnapshot> {
        self.breakpoints
            .breakpoints()
            .iter()
            .map(BreakpointSnapshot::from)
            .collect()
    }

    pub(crate) fn group_breakpoints(&self, group_id: GroupId) -> Vec<BkptMeta> {
        self.breakpoints.group_breakpoints(group_id)
    }

    pub(crate) fn insert_breakpoint(
        &self,
        location: BkptLoc,
        properties: super::BreakpointProperties,
        specs: Vec<SubBkptSpec>,
    ) -> Option<BkptMeta> {
        let breakpoint = self
            .breakpoints
            .insert_breakpoint(location, properties, specs);
        if breakpoint.is_some() {
            self.notify_change(RuntimeChange::breakpoints());
        }
        breakpoint
    }

    pub(crate) fn remove_breakpoint(&self, breakpoint_id: u64) {
        if self.breakpoints.remove_breakpoint(breakpoint_id) {
            self.notify_change(RuntimeChange {
                topology: false,
                breakpoints: true,
                removed: vec![RuntimeResourceId::Breakpoint(breakpoint_id)],
            });
        }
    }

    pub(crate) fn sub_breakpoint(
        &self,
        breakpoint_id: u64,
        sub_breakpoint_id: u64,
    ) -> Option<SubBkptMeta> {
        self.breakpoints
            .sub_breakpoint(breakpoint_id, sub_breakpoint_id)
    }

    pub(crate) fn remove_sub_breakpoint(
        &self,
        breakpoint_id: u64,
        sub_breakpoint_id: u64,
    ) -> BreakpointStateChange {
        let change = self
            .breakpoints
            .remove_sub_breakpoint(breakpoint_id, sub_breakpoint_id);
        if !matches!(&change, BreakpointStateChange::None) {
            self.notify_change(RuntimeChange {
                topology: false,
                breakpoints: true,
                removed: match &change {
                    BreakpointStateChange::Removed(id) => {
                        vec![RuntimeResourceId::Breakpoint(*id)]
                    }
                    _ => Vec::new(),
                },
            });
        }
        change
    }

    pub(crate) fn local_breakpoint_ids(&self, breakpoint_id: u64) -> Vec<(u64, u64)> {
        self.breakpoints.local_breakpoint_ids(breakpoint_id)
    }

    pub(crate) fn update_breakpoint(
        &self,
        breakpoint_id: u64,
        enabled: bool,
        condition: Option<String>,
    ) -> Option<BkptMeta> {
        let breakpoint = self
            .breakpoints
            .update_breakpoint(breakpoint_id, enabled, condition);
        if breakpoint.is_some() {
            self.notify_change(RuntimeChange::breakpoints());
        }
        breakpoint
    }

    pub(crate) fn attach_group_breakpoint_session_target(
        &self,
        breakpoint_id: u64,
        group_id: GroupId,
        sid: u64,
        local_breakpoint_id: u64,
    ) -> BreakpointStateChange {
        let change = self.breakpoints.attach_group_breakpoint_session_target(
            breakpoint_id,
            group_id,
            sid,
            local_breakpoint_id,
        );
        if !matches!(&change, BreakpointStateChange::None) {
            self.notify_change(RuntimeChange::breakpoints());
        }
        change
    }

    pub(crate) fn record_local_breakpoint_deletion(
        &self,
        sid: u64,
        local_breakpoint_id: u64,
    ) -> BreakpointStateChange {
        let change = self
            .breakpoints
            .record_local_bkpt_deletion(sid, local_breakpoint_id);
        if !matches!(&change, BreakpointStateChange::None) {
            self.notify_change(RuntimeChange {
                topology: false,
                breakpoints: true,
                removed: match &change {
                    BreakpointStateChange::Removed(id) => {
                        vec![RuntimeResourceId::Breakpoint(*id)]
                    }
                    _ => Vec::new(),
                },
            });
        }
        change
    }

    pub(crate) fn record_breakpoint_hit(
        &self,
        sid: u64,
        local_breakpoint_id: u64,
    ) -> Option<(u64, u64, BkptMeta)> {
        let hit = self
            .breakpoints
            .record_breakpoint_hit(sid, local_breakpoint_id);
        if hit.is_some() {
            self.notify_change(RuntimeChange::breakpoints());
        }
        hit
    }

    // Proclet ownership.

    pub(crate) fn proclet_owner_session(&self, caladan_ip: u32) -> Option<u64> {
        self.proclets.session_id_for_caladan_ip(caladan_ip)
    }

    pub(crate) fn proclet_owners(&self) -> Vec<(u32, u64)> {
        self.proclets.owners()
    }

    #[cfg(test)]
    pub(crate) fn add_breakpoint(&self, location: BkptLoc) -> u64 {
        let breakpoint_id = self.breakpoints.add_breakpoint(location);
        self.notify_change(RuntimeChange::breakpoints());
        breakpoint_id
    }

    #[cfg(test)]
    pub(crate) fn add_sub_breakpoint(&self, breakpoint_id: u64, kind: SubBkptType) {
        self.breakpoints.add_sub_breakpoint(breakpoint_id, kind);
        self.notify_change(RuntimeChange::breakpoints());
    }
}

/// Effects of retiring a session. The retirement keeps holding the session's
/// group operation gate so callers can publish its state changes before any
/// concurrent group operation interleaves records of its own. Dropping the
/// retirement releases the gate and, for an emptied group, removes the gate
/// entry itself.
#[must_use = "dropping the retirement releases the group gate; publish its changes first"]
pub(crate) struct SessionRetirement {
    model: Arc<RuntimeModel>,
    group_operation: Option<GroupOperationGuard>,
    pub(crate) breakpoint_changes: Vec<BreakpointStateChange>,
    pub(crate) emptied_group: Option<GroupId>,
}

impl Drop for SessionRetirement {
    fn drop(&mut self) {
        drop(self.group_operation.take());
        if let Some(group_id) = self.emptied_group {
            self.model.group_operations.remove_group(group_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{GroupSubBkpt, SubBkptType};

    #[tokio::test]
    async fn change_feed_reports_only_successful_committed_mutations() {
        let model = RuntimeModel::new();
        let mut changes = model.subscribe_changes();

        assert!(model.register_thread_group(7, "i1").await.is_err());
        assert!(changes.try_recv().is_err());

        model.register_session(7, "svc", None).await;
        assert_eq!(changes.recv().await.unwrap(), RuntimeChange::topology());
        assert!(model.register_thread_group(7, "i1").await.is_ok());
        assert_eq!(changes.recv().await.unwrap(), RuntimeChange::topology());

        let breakpoint = model
            .insert_breakpoint(
                BkptLoc::new("main.rs", 7),
                super::super::BreakpointProperties::default(),
                vec![SubBkptSpec::Session {
                    sid: 7,
                    local_id: 1,
                }],
            )
            .unwrap();
        assert_eq!(changes.recv().await.unwrap(), RuntimeChange::breakpoints());
        model.remove_breakpoint(breakpoint.id());
        assert_eq!(
            changes.recv().await.unwrap(),
            RuntimeChange {
                topology: false,
                breakpoints: true,
                removed: vec![RuntimeResourceId::Breakpoint(breakpoint.id())],
            }
        );
        model.remove_breakpoint(breakpoint.id());
        assert!(changes.try_recv().is_err());
        model.remove_breakpoint(u64::MAX);
        assert!(changes.try_recv().is_err());
    }

    async fn model_with_group_breakpoint(sid: u64) -> (Arc<RuntimeModel>, GroupId, u64) {
        let model = RuntimeModel::new();
        let identity = ServiceIdentity::new("hash-a", "api");
        model
            .register_session(sid, "svc", Some(identity.clone()))
            .await;
        drop(model.register_service_group(sid, &identity).await);
        let group_id = model.group_id_by_session(sid).unwrap();
        let breakpoint_id = model.add_breakpoint(BkptLoc::new("main.rs", 7));
        let mut group_breakpoint = GroupSubBkpt::new(group_id);
        group_breakpoint.add_local_bkpt(sid, 1);
        model.add_sub_breakpoint(breakpoint_id, SubBkptType::Group(group_breakpoint));
        model.complete_session_activation(sid, Some(42)).await;
        (model, group_id, breakpoint_id)
    }

    #[tokio::test]
    async fn retiring_the_last_session_reports_the_emptied_group() {
        let (model, group_id, breakpoint_id) = model_with_group_breakpoint(7).await;

        let retirement = model.begin_session_retirement(7).await.finish().await;

        assert_eq!(retirement.emptied_group, Some(group_id));
        assert!(!retirement.breakpoint_changes.is_empty());
        assert!(model.group_by_id(group_id).is_none());
        assert!(model.session_snapshot(7).await.is_none());
        assert_eq!(model.proclet_owner_session(42), None);
        assert!(model.local_breakpoint_ids(breakpoint_id).is_empty());
    }

    #[tokio::test]
    async fn retiring_one_of_two_group_members_keeps_the_group() {
        let (model, group_id, _) = model_with_group_breakpoint(7).await;
        let identity = ServiceIdentity::new("hash-a", "api");
        model
            .register_session(8, "svc-8", Some(identity.clone()))
            .await;
        drop(model.register_service_group(8, &identity).await);

        let retirement = model.begin_session_retirement(7).await.finish().await;

        assert_eq!(retirement.emptied_group, None);
        assert!(model.group_by_id(group_id).is_some());
        assert!(model.session_snapshot(8).await.is_some());
    }

    #[tokio::test]
    async fn activation_publishes_group_membership_only_after_acquiring_the_gate() {
        use std::time::Duration;

        use tokio::time::timeout;

        let (model, group_id, _) = model_with_group_breakpoint(7).await;
        let operation = model.lock_group_operation(group_id).await;
        let identity = ServiceIdentity::new("hash-a", "api");
        model
            .register_session(8, "svc-8", Some(identity.clone()))
            .await;
        let mut activation = tokio::spawn({
            let model = Arc::clone(&model);
            async move { model.register_service_group(8, &identity).await }
        });

        assert!(timeout(Duration::from_millis(20), &mut activation)
            .await
            .is_err());
        assert!(!model
            .group_by_id(group_id)
            .unwrap()
            .session_ids()
            .contains(&8));

        drop(operation);
        let activation_operation = timeout(Duration::from_secs(1), activation)
            .await
            .expect("activation should resume when the group operation completes")
            .expect("activation task should not panic");
        assert!(model
            .group_by_id(group_id)
            .unwrap()
            .session_ids()
            .contains(&8));
        drop(activation_operation);
    }
    #[tokio::test]
    async fn retirement_waits_for_an_in_flight_group_operation() {
        use std::time::Duration;

        use tokio::time::timeout;

        let (model, group_id, _) = model_with_group_breakpoint(7).await;
        let operation = model.lock_group_operation(group_id).await;
        let mut retirement = tokio::spawn({
            let model = Arc::clone(&model);
            async move { model.begin_session_retirement(7).await.finish().await }
        });

        assert!(timeout(Duration::from_millis(20), &mut retirement)
            .await
            .is_err());
        assert!(model.session_snapshot(7).await.is_some());

        drop(operation);
        let result = timeout(Duration::from_secs(1), retirement)
            .await
            .expect("retirement should resume when the group operation completes")
            .expect("retirement task should not panic");
        assert_eq!(result.emptied_group, Some(group_id));
    }

    #[tokio::test]
    async fn retirement_holds_the_group_gate_until_dropped() {
        use std::time::Duration;

        use tokio::time::timeout;

        let (model, group_id, _) = model_with_group_breakpoint(7).await;
        let identity = ServiceIdentity::new("hash-a", "api");
        model
            .register_session(8, "svc-8", Some(identity.clone()))
            .await;
        drop(model.register_service_group(8, &identity).await);

        let retirement = model.begin_session_retirement(7).await.finish().await;
        assert_eq!(retirement.emptied_group, None);

        let mut contender = tokio::spawn({
            let model = Arc::clone(&model);
            async move { drop(model.lock_group_operation(group_id).await) }
        });
        assert!(timeout(Duration::from_millis(20), &mut contender)
            .await
            .is_err());

        drop(retirement);
        timeout(Duration::from_secs(1), contender)
            .await
            .expect("dropping the retirement should release the group gate")
            .expect("contender task should not panic");
    }

    #[tokio::test]
    async fn custom_context_changes_are_only_available_through_model_commands() {
        let model = RuntimeModel::new();
        model.register_session(1, "svc", None).await;
        let context = ThreadContext {
            tid: GlobalThreadId::new(9),
            ctx: std::collections::HashMap::from([("pc".to_string(), 42)]),
        };

        assert!(model.enter_custom_context(1, context.clone()).await);
        let active = model.session_snapshot(1).await.unwrap();
        assert!(active.in_custom_context);
        assert_eq!(active.current_context, Some(context));

        assert!(model.finish_context_restore(1, true).await);
        assert!(!model.session_snapshot(1).await.unwrap().in_custom_context);
    }
}
