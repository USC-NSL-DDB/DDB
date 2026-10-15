use std::sync::Arc;

use tokio::sync::{broadcast, OwnedMutexGuard, OwnedRwLockReadGuard};

use super::{
    bkpt_mgr::{BkptLoc, BkptMeta, BreakpointMgr, BreakpointStateChange, SubBkptSpec},
    bkpt_snapshot::BreakpointSnapshot,
    group_mgr::{GroupMeta, GroupMgr},
    group_operation::GroupOperationCoordinator,
    ids::{GlobalThreadGroupId, GlobalThreadId, GroupId, ServiceIdentity},
    proclet_mgr::ProcletMgr,
    session_mgr::{
        SessionMeta, SessionStatus, ThreadContext, ThreadLocation, ThreadStatus, ThreadStopReason,
    },
    state_mgr::{GlobalThreadIdentity, StateMgr, StateTransitionResult},
    thread_mgr::{LocalThreadId, ThreadIdView},
};

#[cfg(test)]
use super::bkpt_mgr::SubBkptType;

const RUNTIME_CHANGE_QUEUE: usize = 4_096;

/// Keeps the native state validated for dispatch stable until writer admission.
pub(crate) struct ExecutionDispatchPermit {
    sessions: Vec<OwnedRwLockReadGuard<SessionMeta>>,
    claim: Option<StopClaim>,
}

struct StopClaim {
    session: u64,
    thread: u64,
    revision: u64,
    mode: StopClaimMode,
}

#[derive(Clone, Copy)]
enum StopClaimMode {
    Validate,
    Consume,
}

impl ExecutionDispatchPermit {
    pub(crate) fn admit<T, E>(
        &self,
        action: impl FnOnce() -> Result<T, E>,
    ) -> Option<Result<T, E>> {
        if let Some(claim) = &self.claim {
            let session = self
                .sessions
                .iter()
                .find(|session| session.sid() == claim.session)?;
            match claim.mode {
                StopClaimMode::Validate => {
                    session.validate_thread_stop_on_admission(claim.thread, claim.revision, action)
                }
                StopClaimMode::Consume => {
                    session.admit_thread_stop(claim.thread, claim.revision, action)
                }
            }
        } else {
            Some(action())
        }
    }
}

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
    pub(crate) all_threads_running: bool,
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
            all_threads_running: meta.all_threads_running(),
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
    pub(crate) stop_revision: u64,
    pub(crate) location: Option<ThreadLocation>,
    pub(crate) stop_reason: Option<ThreadStopReason>,
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
    execution_control_epoch: Arc<super::ExecutionControl>,
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
            execution_control_epoch: Arc::new(super::ExecutionControl::default()),
        })
    }

    pub(crate) fn subscribe_changes(&self) -> broadcast::Receiver<RuntimeChange> {
        self.changes.subscribe()
    }

    pub(crate) fn execution_control_epoch(&self) -> Arc<super::ExecutionControl> {
        self.execution_control_epoch.clone()
    }

    pub(crate) fn retire_stop_automation(&self) {
        self.retire_stop_automation_for_pauses(&[]);
    }

    pub(crate) fn retire_stop_automation_for_pauses(&self, sessions: &[u64]) {
        self.execution_control_epoch.advance_for_pauses(sessions);
        self.notify_change(RuntimeChange::topology());
    }

    pub(crate) fn retire_stop_automation_for_resumes(&self, sessions: &[u64]) {
        self.execution_control_epoch.advance_for_resumes(sessions);
        self.notify_change(RuntimeChange::topology());
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

    /// Consume a specific observed stop while holding the session state lock.
    /// Two clients cannot dispatch guarded actions for the same stop twice.
    pub(crate) async fn claim_thread_stop(&self, thread: GlobalThreadId, revision: u64) -> bool {
        self.prepare_stop_for_dispatch(thread, revision)
            .await
            .and_then(|permit| permit.admit(|| Ok::<(), std::convert::Infallible>(())))
            .is_some()
    }

    pub(crate) async fn prepare_stop_for_dispatch(
        &self,
        thread: GlobalThreadId,
        revision: u64,
    ) -> Option<ExecutionDispatchPermit> {
        self.prepare_stop_claim(thread, revision, StopClaimMode::Consume)
            .await
    }

    pub(crate) async fn prepare_stop_prerequisite(
        &self,
        thread: GlobalThreadId,
        revision: u64,
    ) -> Option<ExecutionDispatchPermit> {
        self.prepare_stop_claim(thread, revision, StopClaimMode::Validate)
            .await
    }

    /// Inspection reads bind to both the native stop and register context.
    /// The prerequisite permit retains the same state through writer admission.
    pub(crate) async fn prepare_stop_read(
        &self,
        thread: GlobalThreadId,
        native_revision: u64,
        inspection_revision: u64,
    ) -> Option<ExecutionDispatchPermit> {
        let permit = self
            .prepare_stop_prerequisite(thread, native_revision)
            .await?;
        let claim = permit.claim.as_ref()?;
        let session = permit
            .sessions
            .iter()
            .find(|session| session.sid() == claim.session)?;
        if session.is_in_custom_context()
            && session
                .current_context()
                .is_some_and(|context| context.tid == thread)
        {
            return None;
        }
        let current = session.thread_status_snapshot().1;
        if !current.iter().any(|state| {
            state.local_id == claim.thread && state.execution_revision == inspection_revision
        }) {
            return None;
        }
        Some(permit)
    }

    async fn prepare_stop_claim(
        &self,
        thread: GlobalThreadId,
        revision: u64,
        mode: StopClaimMode,
    ) -> Option<ExecutionDispatchPermit> {
        let LocalThreadId(sid, tid) = self.local_thread_id(thread)?;
        let mut permit = self
            .validate_stop_for_dispatch(thread, revision, None)
            .await?;
        permit.claim = Some(StopClaim {
            session: sid,
            thread: tid,
            revision,
            mode,
        });
        Some(permit)
    }

    /// Lock sessions in identity order, then validate their state together. This
    /// avoids mixing an old initiating stop with a newly sampled peer state.
    pub(crate) async fn validate_stop_for_dispatch(
        &self,
        thread: GlobalThreadId,
        revision: u64,
        peer: Option<(u64, &[(u64, u64)])>,
    ) -> Option<ExecutionDispatchPermit> {
        let LocalThreadId(sid, tid) = self.local_thread_id(thread)?;
        let mut ids = vec![sid];
        if let Some((peer, _)) = peer {
            ids.push(peer);
        }
        ids.sort_unstable();
        ids.dedup();
        let mut sessions = Vec::new();
        for id in ids {
            sessions.push(self.state.session(id)?.read_owned().await);
        }
        // Retirement marks OFF under the same exclusive lock before removing
        // indexes. A newly prepared permit must reject that retirement window.
        if sessions
            .iter()
            .any(|session| session.status() != SessionStatus::ON)
        {
            return None;
        }
        // The local debugger ID may have been removed and reused while these
        // locks were awaited. Native revisions alone do not identify that thread.
        if self.state.global_thread_id(sid, tid) != Some(thread) {
            return None;
        }
        let current = sessions
            .iter()
            .find(|session| session.sid() == sid)?
            .thread_status_snapshot()
            .1;
        if !current.iter().any(|state| {
            state.local_id == tid
                && state.status == ThreadStatus::STOPPED
                && state.stop_revision == revision
        }) {
            return None;
        }
        if let Some((peer, expected)) = peer {
            let mut running = sessions
                .iter()
                .find(|session| session.sid() == peer)?
                .thread_status_snapshot()
                .1
                .iter()
                .filter(|state| state.status == ThreadStatus::RUNNING)
                .filter_map(|state| {
                    self.state
                        .global_thread_id(peer, state.local_id)
                        .map(|id| (id.value(), state.stop_revision))
                })
                .collect::<Vec<_>>();
            running.sort_unstable();
            if running != expected {
                return None;
            }
        }
        Some(ExecutionDispatchPermit {
            sessions,
            claim: None,
        })
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
                        stop_revision: thread.stop_revision,
                        location: thread.location,
                        stop_reason: thread.stop_reason,
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
        self.execution_control_epoch.forget_session(sid);
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
            .with_session_mut(sid, |session| {
                if let Some(owner) = self
                    .state
                    .local_thread_id(context.tid)
                    .filter(|owner| owner.session_id() == sid)
                {
                    session.invalidate_thread_context(owner.thread_id());
                }
                session.enter_custom_context(context);
            })
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
            .with_session_mut(sid, |session| {
                if restored && session.is_in_custom_context() {
                    if let Some(owner) = session
                        .current_context()
                        .and_then(|context| self.state.local_thread_id(context.tid))
                        .filter(|owner| owner.session_id() == sid)
                    {
                        session.invalidate_thread_context(owner.thread_id());
                    }
                }
                session.exit_custom_context(restored);
            })
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

    pub(crate) async fn update_thread_statuses_with_details(
        &self,
        sid: u64,
        local_thread_ids: &[u64],
        status: ThreadStatus,
        location: Option<(u64, ThreadLocation)>,
        mut stop_reason: Option<ThreadStopReason>,
    ) -> StateTransitionResult<()> {
        // Record observation before awaiting state ownership. A later explicit
        // intent also retires native stops still waiting to be classified.
        if let Some(reason) = stop_reason.as_mut() {
            reason.observed_control_epoch = self.execution_control_epoch.current();
        }
        let result = self
            .state
            .update_thread_statuses_with_details(
                sid,
                local_thread_ids,
                status,
                location,
                stop_reason,
            )
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

    #[cfg(test)]
    pub(crate) fn remove_breakpoint(&self, breakpoint_id: u64) {
        if self.breakpoints.remove_breakpoint(breakpoint_id) {
            self.notify_change(RuntimeChange {
                topology: false,
                breakpoints: true,
                removed: vec![RuntimeResourceId::Breakpoint(breakpoint_id)],
            });
        }
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
        log_message: Option<String>,
    ) -> Option<BkptMeta> {
        let breakpoint =
            self.breakpoints
                .update_breakpoint(breakpoint_id, enabled, condition, log_message);
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
    async fn peer_dispatch_validates_a_consistent_cause_while_waiting_for_peer_state() {
        let model = RuntimeModel::new();
        let mut cause = None;
        for sid in [1, 2] {
            model.register_session(sid, "worker", None).await;
            model.complete_session_activation(sid, None).await;
            model.register_thread_group(sid, "i1").await.unwrap();
            let thread = model.register_thread(sid, 1, "i1").await.unwrap().thread_id;
            model
                .update_thread_statuses(
                    sid,
                    &[1],
                    if sid == 1 {
                        ThreadStatus::STOPPED
                    } else {
                        ThreadStatus::RUNNING
                    },
                )
                .await
                .unwrap();
            if sid == 1 {
                cause = Some(thread);
            }
        }
        let cause = cause.unwrap();
        let revision = model.thread_snapshots_for_sessions(&[1]).await[0].stop_revision;
        let peer = model.thread_snapshots_for_sessions(&[2]).await[0].clone();
        let expected = [(peer.global_id.value(), peer.stop_revision)];
        let peer_session = model.state.session(2).unwrap();
        let peer_busy = peer_session.write().await;
        let mut validation =
            Box::pin(model.validate_stop_for_dispatch(cause, revision, Some((2, &expected))));
        assert!(futures::poll!(validation.as_mut()).is_pending());
        let mut intervening_stop =
            Box::pin(model.update_thread_statuses(1, &[1], ThreadStatus::STOPPED));
        assert!(
            futures::poll!(intervening_stop.as_mut()).is_pending(),
            "the cause must remain locked while peer state is awaited"
        );
        drop(peer_busy);
        let permit = validation
            .await
            .expect("the locked native generation is still current");
        drop(permit);
        intervening_stop.await.unwrap();
        assert!(model
            .validate_stop_for_dispatch(cause, revision, Some((2, &expected)))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn dispatch_rejects_a_reused_local_thread_id_after_waiting_for_peer_state() {
        let model = RuntimeModel::new();
        for sid in [1, 2] {
            model.register_session(sid, "worker", None).await;
            model.complete_session_activation(sid, None).await;
            model.register_thread_group(sid, "i1").await.unwrap();
            model.register_thread(sid, 1, "i1").await.unwrap();
            model
                .update_thread_statuses(
                    sid,
                    &[1],
                    if sid == 1 {
                        ThreadStatus::RUNNING
                    } else {
                        ThreadStatus::STOPPED
                    },
                )
                .await
                .unwrap();
        }
        let old = model.thread_snapshots_for_sessions(&[2]).await.remove(0);
        let peer = model.thread_snapshots_for_sessions(&[1]).await.remove(0);
        let running = [(peer.global_id.value(), peer.stop_revision)];
        let peer_session = model.state.session(1).unwrap();
        let peer_busy = peer_session.write().await;
        let mut validation = Box::pin(model.validate_stop_for_dispatch(
            old.global_id,
            old.stop_revision,
            Some((1, &running)),
        ));
        assert!(futures::poll!(validation.as_mut()).is_pending());
        model.remove_thread(2, 1, "i1").await.unwrap();
        let replacement = model.register_thread(2, 1, "i1").await.unwrap().thread_id;
        model
            .update_thread_statuses(2, &[1], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let new = model.thread_snapshots_for_sessions(&[2]).await.remove(0);
        assert_ne!(replacement, old.global_id);
        assert_eq!(new.stop_revision, old.stop_revision);
        drop(peer_busy);
        assert!(
            validation.await.is_none(),
            "a recycled local ID is a different thread"
        );
        assert!(model
            .prepare_stop_for_dispatch(new.global_id, new.stop_revision)
            .await
            .is_some());
    }

    #[tokio::test]
    async fn dispatch_rejects_an_inactive_cause_or_peer_before_indexes_are_removed() {
        let model = RuntimeModel::new();
        for sid in [1, 2] {
            model.register_session(sid, "worker", None).await;
            model.complete_session_activation(sid, None).await;
            model.register_thread_group(sid, "i1").await.unwrap();
            model.register_thread(sid, 1, "i1").await.unwrap();
            model
                .update_thread_statuses(
                    sid,
                    &[1],
                    if sid == 1 {
                        ThreadStatus::STOPPED
                    } else {
                        ThreadStatus::RUNNING
                    },
                )
                .await
                .unwrap();
        }
        let cause = model.thread_snapshots_for_sessions(&[1]).await.remove(0);
        let peer = model.thread_snapshots_for_sessions(&[2]).await.remove(0);
        let running = [(peer.global_id.value(), peer.stop_revision)];
        for inactive in [1, 2] {
            model.state.update_session_status_off(inactive).await;
            assert!(model
                .validate_stop_for_dispatch(
                    cause.global_id,
                    cause.stop_revision,
                    Some((2, &running))
                )
                .await
                .is_none());
            if inactive == 1 {
                assert!(model
                    .prepare_stop_for_dispatch(cause.global_id, cause.stop_revision)
                    .await
                    .is_none());
            }
            model.state.update_session_status_on(inactive).await;
        }
        assert!(model
            .prepare_stop_for_dispatch(cause.global_id, cause.stop_revision)
            .await
            .is_some());
    }

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
    async fn context_restore_expires_only_its_owner_without_waiting_for_native_events() {
        let model = RuntimeModel::new();
        model.register_session(1, "caller", None).await;
        model.register_thread_group(1, "i1").await.unwrap();
        let owner = model.register_thread(1, 10, "i1").await.unwrap().thread_id;
        let peer = model.register_thread(1, 20, "i1").await.unwrap().thread_id;
        model
            .update_thread_statuses(1, &[10, 20], ThreadStatus::STOPPED)
            .await
            .unwrap();
        model
            .enter_custom_context(
                1,
                ThreadContext {
                    tid: owner,
                    ctx: std::collections::HashMap::from([("pc".to_string(), 42)]),
                },
            )
            .await;
        let before = model.thread_snapshots_for_sessions(&[1]).await;

        model.finish_context_restore(1, false).await;
        assert!(model.session_snapshot(1).await.unwrap().in_custom_context);
        assert_eq!(model.thread_snapshots_for_sessions(&[1]).await, before);

        model.finish_context_restore(1, true).await;
        let after = model.thread_snapshots_for_sessions(&[1]).await;
        let thread = |threads: &[ThreadSnapshot], id| {
            threads
                .iter()
                .find(|thread| thread.global_id == id)
                .unwrap()
                .clone()
        };
        assert!(
            thread(&after, owner).execution_revision > thread(&before, owner).execution_revision,
            "restoring registers must expire the reconstructed caller's frame handles"
        );
        assert_eq!(thread(&after, peer), thread(&before, peer));
        assert!(after
            .iter()
            .all(|thread| thread.status == ThreadStatus::STOPPED));

        model.finish_context_restore(1, true).await;
        assert_eq!(model.thread_snapshots_for_sessions(&[1]).await, after);
    }

    #[tokio::test]
    async fn context_entry_expires_only_its_owners_reads_and_preserves_native_claims() {
        let model = RuntimeModel::new();
        model.register_session(1, "caller", None).await;
        model.complete_session_activation(1, None).await;
        model.register_thread_group(1, "i1").await.unwrap();
        let owner = model.register_thread(1, 10, "i1").await.unwrap().thread_id;
        let peer = model.register_thread(1, 20, "i1").await.unwrap().thread_id;
        model
            .update_thread_statuses(1, &[10, 20], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let before = model.thread_snapshots_for_sessions(&[1]).await;
        let owner_before = before
            .iter()
            .find(|thread| thread.global_id == owner)
            .unwrap();
        let peer_before = before
            .iter()
            .find(|thread| thread.global_id == peer)
            .unwrap();
        assert!(model
            .prepare_stop_read(
                owner,
                owner_before.stop_revision,
                owner_before.execution_revision
            )
            .await
            .is_some());

        assert!(
            model
                .enter_custom_context(
                    1,
                    ThreadContext {
                        tid: owner,
                        ctx: std::collections::HashMap::from([("pc".into(), 42)]),
                    }
                )
                .await
        );
        let after = model.thread_snapshots_for_sessions(&[1]).await;
        let owner_after = after
            .iter()
            .find(|thread| thread.global_id == owner)
            .unwrap();
        let peer_after = after
            .iter()
            .find(|thread| thread.global_id == peer)
            .unwrap();
        assert!(owner_after.execution_revision > owner_before.execution_revision);
        assert_eq!(owner_after.stop_revision, owner_before.stop_revision);
        assert_eq!(owner_after.status, ThreadStatus::STOPPED);
        assert_eq!(peer_after, peer_before);
        assert!(model
            .prepare_stop_read(
                owner,
                owner_before.stop_revision,
                owner_before.execution_revision
            )
            .await
            .is_none());
        assert!(model
            .prepare_stop_read(
                owner,
                owner_after.stop_revision,
                owner_after.execution_revision
            )
            .await
            .is_none());
        assert!(model
            .prepare_stop_prerequisite(owner, owner_after.stop_revision)
            .await
            .unwrap()
            .admit(|| Ok::<_, std::convert::Infallible>(()))
            .is_some());
        assert!(model
            .prepare_stop_read(
                peer,
                peer_before.stop_revision,
                peer_before.execution_revision
            )
            .await
            .is_some());
        assert!(model.finish_context_restore(1, true).await);
        let restored = model.thread_snapshots_for_sessions(&[1]).await;
        let owner_restored = restored
            .iter()
            .find(|thread| thread.global_id == owner)
            .unwrap();
        assert_eq!(owner_restored.stop_revision, owner_before.stop_revision);
        assert!(model
            .prepare_stop_read(
                owner,
                owner_restored.stop_revision,
                owner_restored.execution_revision
            )
            .await
            .is_some());
        assert!(
            model
                .claim_thread_stop(owner, owner_before.stop_revision)
                .await
        );
    }

    #[tokio::test]
    async fn custom_context_changes_are_only_available_through_model_commands() {
        let model = RuntimeModel::new();
        model.register_session(1, "svc", None).await;
        model.register_thread_group(1, "i1").await.unwrap();
        let owner = model.register_thread(1, 9, "i1").await.unwrap().thread_id;
        let context = ThreadContext {
            tid: owner,
            ctx: std::collections::HashMap::from([("pc".to_string(), 42)]),
        };

        assert!(model.enter_custom_context(1, context.clone()).await);
        let active = model.session_snapshot(1).await.unwrap();
        assert!(active.in_custom_context);
        assert_eq!(active.current_context, Some(context));

        assert!(model.finish_context_restore(1, true).await);
        assert!(!model.session_snapshot(1).await.unwrap().in_custom_context);
    }

    #[tokio::test]
    async fn native_stop_observation_keeps_its_epoch_while_waiting_for_state_ownership() {
        let model = RuntimeModel::new();
        model.register_session(1, "native-stop", None).await;
        model.complete_session_activation(1, None).await;
        model.register_thread_group(1, "i1").await.unwrap();
        let owner = model.register_thread(1, 10, "i1").await.unwrap().thread_id;
        model
            .update_thread_statuses(1, &[10], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let before = model.thread_snapshots_for_sessions(&[1]).await[0].clone();
        let permit = model
            .validate_stop_for_dispatch(owner, before.stop_revision, None)
            .await
            .unwrap();
        let epoch = model.execution_control_epoch().current();
        let mut observation = Box::pin(model.update_thread_statuses_with_details(
            1,
            &[10],
            ThreadStatus::STOPPED,
            None,
            Some(ThreadStopReason {
                observed_control_epoch: u64::MAX,
                kind: crate::state::ThreadStopKind::Pause,
                signal_name: None,
                breakpoint_id: None,
                thread_id: Some(owner.value()),
                log_message: None,
            }),
        ));
        assert!(futures::poll!(observation.as_mut()).is_pending());
        model.retire_stop_automation();
        assert!(model.execution_control_epoch().retirement_floor() > epoch);
        drop(permit);
        observation.await.unwrap();
        let after = model.thread_snapshots_for_sessions(&[1]).await[0].clone();
        let reason = after.stop_reason.unwrap();
        assert_eq!(reason.observed_control_epoch, epoch);
        assert!(!serde_json::to_string(&reason)
            .unwrap()
            .contains("observed_control_epoch"));
        assert!(after.stop_revision > before.stop_revision);
    }
}
