//! One backend owns stop classification, logpoint work, and peer pause attempts.
//! Client execution retires pending automation. Every delayed command checks its
//! generation again in the session actor, after preceding events have committed.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{anyhow, ensure, Context};
use ddb_api_types::v2::{self, operation_result, resource_upsert, state_event, target};
use tokio::{
    sync::{broadcast, Notify},
    task::JoinSet,
};

use super::mutations::CompletionProjection;
use super::{
    ApplicationError, DdbApplicationService, PrincipalContext, RequestScope, ResolvedTarget,
    ResourceIdKind, StateChange, StateEventContext,
};
use crate::{
    api::read_model::ThreadView,
    cmd_flow::{
        input::{DispatchGuard, DispatchPermit, StaleExecution},
        logpoint_template::{Part, Template, MAX_TEMPLATE_BYTES},
        output_hub::{DebuggerOutputStream, OutputHub},
        router::Target as CommandTarget,
    },
    state::{ExecutionControl, GlobalThreadId, RuntimeModel, ThreadStopKind},
};

const MAX_AUTOMATIC_TASKS: usize = 16;
const AUTOMATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct Stop {
    thread: ThreadView,
    execution: v2::ExecutionState,
    epoch: u64,
}

struct DispatchedPauses {
    revision: u64,
    attempts: HashMap<u64, u64>,
    // Completed acknowledgments and ambiguous failures need only their newest
    // control epoch. Unsettled attempts remain separate for precise rejection.
    settled_epoch: Option<u64>,
}

impl DispatchedPauses {
    fn latest_epoch(&self) -> Option<u64> {
        self.attempts
            .values()
            .copied()
            .chain(self.settled_epoch)
            .max()
    }
}

#[derive(Clone, Copy)]
struct ManualStop {
    revision: u64,
    epoch: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LogpointDisposition {
    InFlight,
    Continued,
    Failed,
}

#[derive(Clone, Copy)]
struct LogpointState {
    revision: u64,
    epoch: u64,
    disposition: LogpointDisposition,
}

struct PolicyState {
    public: v2::StopPolicy,
    seen: HashMap<u64, u64>,
    logpoints: HashMap<u64, LogpointState>,
    ordinary_stops: BTreeMap<u64, Stop>,
    manual_stops: HashMap<u64, ManualStop>,
    // Preserve observed provenance independently of pending command receipts.
    automatic_stops: HashMap<u64, u64>,
    cause: Option<Stop>,
    attempts: BTreeMap<u64, Vec<(u64, u64)>>,
    next_attempt: u64,
    automatic_pauses: HashMap<u64, DispatchedPauses>,
}

pub(super) struct StopPolicies {
    state: Arc<Mutex<PolicyState>>,
    epoch: Arc<ExecutionControl>,
    wake: Arc<Notify>,
}

impl StopPolicies {
    #[cfg(test)]
    pub fn classified_stop(&self, thread: u64, revision: u64) -> bool {
        self.state().seen.get(&thread) == Some(&revision)
    }

    #[cfg(test)]
    pub fn continued_logpoint(&self, thread: u64, revision: u64) -> bool {
        self.state().logpoints.get(&thread).is_some_and(|stop| {
            stop.revision == revision && stop.disposition == LogpointDisposition::Continued
        })
    }

    pub fn new(epoch: Arc<ExecutionControl>) -> Self {
        Self {
            state: Arc::new(Mutex::new(PolicyState {
                public: v2::StopPolicy {
                    mode: v2::StopMode::Independent as i32,
                    revision: 1,
                    ..Default::default()
                },
                seen: HashMap::new(),
                logpoints: HashMap::new(),
                ordinary_stops: BTreeMap::new(),
                manual_stops: HashMap::new(),
                automatic_stops: HashMap::new(),
                cause: None,
                attempts: BTreeMap::new(),
                next_attempt: 0,
                automatic_pauses: HashMap::new(),
            })),
            epoch,
            wake: Arc::new(Notify::new()),
        }
    }

    fn state(&self) -> MutexGuard<'_, PolicyState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[derive(Debug)]
struct ActiveGuard {
    inner: Arc<dyn DispatchGuard>,
    active: Arc<Mutex<bool>>,
}

struct AutomationLease(Arc<Mutex<bool>>);
impl Drop for AutomationLease {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = false;
    }
}

#[async_trait::async_trait]
impl DispatchGuard for ActiveGuard {
    async fn prepare_prerequisite(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        if !*self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            return Err(StaleExecution.into());
        }
        let inner = self.inner.prepare_prerequisite().await?;
        Ok(Box::new(ActivePermit {
            inner,
            active: self.active.clone(),
        }))
    }

    async fn prepare(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        if !*self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            return Err(StaleExecution.into());
        }
        let inner = self.inner.prepare().await?;
        Ok(Box::new(ActivePermit {
            inner,
            active: self.active.clone(),
        }))
    }
    fn is_automatic(&self) -> bool {
        true
    }
    fn dispatched(&self) {
        self.inner.dispatched();
    }
    fn rejected(&self) {
        self.inner.rejected();
    }
    fn finished(&self) {
        self.inner.finished();
    }
    async fn check(&self) -> anyhow::Result<()> {
        if !*self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            return Err(StaleExecution.into());
        }
        self.inner.check().await?;
        if !*self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            return Err(StaleExecution.into());
        }
        Ok(())
    }
}

struct ActivePermit {
    inner: Box<dyn DispatchPermit>,
    active: Arc<Mutex<bool>>,
}

impl DispatchPermit for ActivePermit {
    fn dispatch(
        &mut self,
        validate: &mut dyn FnMut() -> anyhow::Result<()>,
        action: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*active {
            return Err(StaleExecution.into());
        }
        self.inner.dispatch(validate, action)
    }
}

struct PolicyPermit {
    native: Box<dyn DispatchPermit>,
    epoch: Arc<ExecutionControl>,
    expected: u64,
    output: Option<(Arc<OutputHub>, u64, u64, String)>,
}

impl DispatchPermit for PolicyPermit {
    fn dispatch(
        &mut self,
        validate: &mut dyn FnMut() -> anyhow::Result<()>,
        action: &mut dyn FnMut() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.epoch
            .admit(self.expected, || {
                self.native.dispatch(validate, &mut || {
                    if let Some((hub, session, thread, message)) = &self.output {
                        hub.publish_on_admission(
                            Some(*session),
                            Some(*thread),
                            DebuggerOutputStream::Console,
                            message.clone(),
                            &mut *action,
                        )
                    } else {
                        action()
                    }
                })
            })
            .ok_or(StaleExecution)?
    }
}

// This guard checks the initiating stop as well as the policy generation. It
// never consumes the cause because several peer sessions can require a pause.
struct PeerPauseGuard {
    model: Arc<RuntimeModel>,
    epoch: Arc<ExecutionControl>,
    stop: Stop,
    peer: u64,
    running: Vec<(u64, u64)>,
    attempt: u64,
    state: Arc<Mutex<PolicyState>>,
    wake: Arc<Notify>,
}

impl std::fmt::Debug for PeerPauseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerPauseGuard")
            .field("peer", &self.peer)
            .finish()
    }
}

#[async_trait::async_trait]
impl DispatchGuard for PeerPauseGuard {
    async fn prepare(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        let native = self
            .model
            .validate_stop_for_dispatch(
                GlobalThreadId::new(self.stop.thread.global_id),
                self.stop.thread.stop_revision,
                Some((self.peer, &self.running)),
            )
            .await
            .ok_or(StaleExecution)?;
        if self.epoch.current() != self.stop.epoch {
            return Err(StaleExecution.into());
        }
        Ok(Box::new(PolicyPermit {
            native: Box::new(native),
            epoch: self.epoch.clone(),
            expected: self.stop.epoch,
            output: None,
        }))
    }
    async fn check(&self) -> anyhow::Result<()> {
        self.prepare().await.map(|_| ())
    }
    fn dispatched(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (thread, revision) in &self.running {
            let revision = revision.saturating_add(1);
            let pauses =
                state
                    .automatic_pauses
                    .entry(*thread)
                    .or_insert_with(|| DispatchedPauses {
                        revision,
                        attempts: HashMap::new(),
                        settled_epoch: None,
                    });
            if pauses.revision != revision {
                *pauses = DispatchedPauses {
                    revision,
                    attempts: HashMap::new(),
                    settled_epoch: None,
                };
            }
            pauses.attempts.insert(self.attempt, self.stop.epoch);
        }
    }
    fn rejected(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for (thread, revision) in &self.running {
            let revision = revision.saturating_add(1);
            let Some(pauses) = state
                .automatic_pauses
                .get_mut(thread)
                .filter(|pauses| pauses.revision == revision)
            else {
                continue;
            };
            pauses.attempts.remove(&self.attempt);
            let latest_epoch = pauses.latest_epoch();
            let manual_epoch = state
                .manual_stops
                .get(thread)
                .filter(|stop| stop.revision == revision)
                .map(|stop| stop.epoch);
            if latest_epoch.is_none() {
                state.automatic_pauses.remove(thread);
            }
            if state.automatic_stops.get(thread) == Some(&revision)
                && latest_epoch.is_none_or(|automatic_epoch| {
                    manual_epoch.is_some_and(|manual_epoch| manual_epoch >= automatic_epoch)
                })
            {
                state.automatic_stops.remove(thread);
                state.seen.remove(thread);
            }
        }
        self.wake.notify_one();
    }
    fn finished(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for (thread, revision) in &self.running {
            if let Some(pauses) = state.automatic_pauses.get_mut(thread) {
                if pauses.revision == revision.saturating_add(1) {
                    if let Some(epoch) = pauses.attempts.remove(&self.attempt) {
                        pauses.settled_epoch = Some(pauses.settled_epoch.unwrap_or(0).max(epoch));
                    }
                }
            }
        }
    }
}

struct LogpointReadGuard {
    model: Arc<RuntimeModel>,
    epoch: Arc<ExecutionControl>,
    stop: Stop,
}

impl std::fmt::Debug for LogpointReadGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogpointReadGuard")
            .field("thread", &self.stop.thread.global_id)
            .finish()
    }
}

#[async_trait::async_trait]
impl DispatchGuard for LogpointReadGuard {
    async fn prepare(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        let native = self
            .model
            .prepare_stop_read(
                GlobalThreadId::new(self.stop.thread.global_id),
                self.stop.thread.stop_revision,
                self.stop.thread.execution_revision,
            )
            .await
            .ok_or(StaleExecution)?;
        if self.epoch.current() != self.stop.epoch {
            return Err(StaleExecution.into());
        }
        Ok(Box::new(PolicyPermit {
            native: Box::new(native),
            epoch: self.epoch.clone(),
            expected: self.stop.epoch,
            output: None,
        }))
    }
    async fn check(&self) -> anyhow::Result<()> {
        self.prepare().await.map(|_| ())
    }
}

struct LogpointGuard {
    stop: Arc<dyn DispatchGuard>,
    epoch: Arc<ExecutionControl>,
    expected: u64,
    output: Arc<OutputHub>,
    session: u64,
    thread: u64,
    message: String,
}

impl std::fmt::Debug for LogpointGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogpointGuard")
            .field("session", &self.session)
            .finish()
    }
}

#[async_trait::async_trait]
impl DispatchGuard for LogpointGuard {
    async fn prepare_prerequisite(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        if self.epoch.current() != self.expected {
            return Err(StaleExecution.into());
        }
        let native = self.stop.prepare_prerequisite().await?;
        if self.epoch.current() != self.expected {
            return Err(StaleExecution.into());
        }
        Ok(Box::new(PolicyPermit {
            native,
            epoch: self.epoch.clone(),
            expected: self.expected,
            output: None,
        }))
    }

    async fn prepare(&self) -> anyhow::Result<Box<dyn DispatchPermit>> {
        if self.epoch.current() != self.expected {
            return Err(StaleExecution.into());
        }
        let native = self.stop.prepare().await?;
        if self.epoch.current() != self.expected {
            return Err(StaleExecution.into());
        }
        Ok(Box::new(PolicyPermit {
            native,
            epoch: self.epoch.clone(),
            expected: self.expected,
            output: Some((
                self.output.clone(),
                self.session,
                self.thread,
                self.message.clone(),
            )),
        }))
    }
    async fn check(&self) -> anyhow::Result<()> {
        self.prepare().await.map(|_| ())
    }
}

impl DdbApplicationService {
    pub(super) fn stop_policy_snapshot(&self) -> Result<v2::StopPolicy, ApplicationError> {
        let mut public = self.stop_policies.state().public.clone();
        public.stop_policy_id = self.ids.encode(ResourceIdKind::StopPolicy, "current")?;
        Ok(public)
    }

    fn publish_stop_policy(&self, state: &mut PolicyState) -> Result<(), ApplicationError> {
        state.public.revision += 1;
        let mut public = state.public.clone();
        public.stop_policy_id = self.ids.encode(ResourceIdKind::StopPolicy, "current")?;
        self.journal.publish(StateChange {
            request_id: None,
            operation_id: None,
            kind: v2::StateEventKind::ResourceUpserted,
            resource_kind: v2::ResourceKind::StopPolicy,
            resource_id: public.stop_policy_id.clone(),
            resource_revision: public.revision,
            payload: state_event::Payload::Upsert(v2::ResourceUpsert {
                resource: Some(resource_upsert::Resource::StopPolicy(public)),
            }),
            extension_details: Vec::new(),
            context: StateEventContext {
                global: true,
                ..Default::default()
            },
        })?;
        Ok(())
    }

    pub(super) fn retire_stop_automation(&self) {
        self.retire_stop_automation_for_pauses(&[]);
    }

    pub(super) fn retire_stop_automation_for_pauses(&self, sessions: &[u64]) {
        self.retire_stop_automation_with(|| self.stop_policies.epoch.advance_for_pauses(sessions));
    }

    pub(super) fn retire_stop_automation_for_resumes(&self, sessions: &[u64]) {
        self.retire_stop_automation_with(|| self.stop_policies.epoch.advance_for_resumes(sessions));
    }

    fn retire_stop_automation_with(&self, advance: impl FnOnce()) {
        let mut state = self.stop_policies.state();
        advance();
        state.cause = None;
        state.ordinary_stops.clear();
        state.attempts.clear();
        state.public.generation += 1;
        state.public.initiating_stop = None;
        state.public.pending_targets.clear();
        state.public.target_outcomes.clear();
        let _ = self.publish_stop_policy(&mut state);
        self.stop_policies.wake.notify_one();
    }

    #[cfg(test)]
    pub(super) fn configure_stop_policy(&self, mode: v2::StopMode) -> Result<(), ApplicationError> {
        self.configure_stop_policy_checked(mode, None)
    }

    pub(super) fn configure_stop_policy_checked(
        &self,
        mode: v2::StopMode,
        expected: Option<&v2::ResourceVersion>,
    ) -> Result<(), ApplicationError> {
        let mut state = self.stop_policies.state();
        if let Some(expected) = expected {
            if expected.revision != state.public.revision
                || expected.resource_id != self.ids.encode(ResourceIdKind::StopPolicy, "current")?
            {
                return Err(ApplicationError::new(
                    v2::DdbErrorCode::FailedPrecondition,
                    "stop policy changed since it was observed",
                ));
            }
        }
        if state.public.mode == mode as i32 && state.public.generation != 0 {
            return Ok(());
        }
        self.stop_policies.epoch.reconfigure();
        state.cause = None;
        state.ordinary_stops.clear();
        for logpoint in state.logpoints.values_mut() {
            if logpoint.disposition == LogpointDisposition::InFlight {
                logpoint.disposition = LogpointDisposition::Failed;
            }
        }
        let logpoints = state.logpoints.clone();
        let manual_stops = state.manual_stops.clone();
        let automatic_stops = state.automatic_stops.clone();
        state.seen.retain(|id, revision| {
            (logpoints.get(id).is_some_and(|stop| {
                stop.revision == *revision && stop.disposition == LogpointDisposition::Continued
            }) && manual_stops.get(id).map(|stop| stop.revision) != Some(*revision))
                || automatic_stops.get(id) == Some(revision)
        });
        state.attempts.clear();
        state.public.mode = mode as i32;
        state.public.generation += 1;
        state.public.initiating_stop = None;
        state.public.pending_targets.clear();
        state.public.target_outcomes.clear();
        self.publish_stop_policy(&mut state)?;
        self.stop_policies.wake.notify_one();
        Ok(())
    }

    pub(super) fn spawn_stop_policy_worker(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let mut changes = self.queries.subscribe_runtime_changes();
        let weak = Arc::downgrade(self);
        let wake = self.stop_policies.wake.clone();
        tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                if let Some(service) = weak.upgrade() {
                    if let Err(error) = service.reconcile_stop_policies(&mut tasks).await {
                        tracing::warn!(code = ?error.code(), "stop policy reconciliation failed");
                    }
                } else {
                    return;
                }
                tokio::select! {
                    change = changes.recv() => if matches!(change, Err(broadcast::error::RecvError::Closed)) { return },
                    _ = wake.notified() => {},
                    result = tasks.join_next(), if !tasks.is_empty() => {
                        if let Some(Err(error)) = result { tracing::warn!(?error, "stop policy task failed"); }
                    }
                }
            }
        })
    }

    async fn reconcile_stop_policies(
        self: &Arc<Self>,
        tasks: &mut JoinSet<()>,
    ) -> Result<(), ApplicationError> {
        let epoch = self.stop_policies.epoch.current();
        let view = self.queries.snapshot().await;
        let mut logpoints = Vec::new();
        let mut pauses = Vec::new();
        let publication;
        {
            let mut state = self.stop_policies.state();
            if self.stop_policies.epoch.current() != epoch {
                return Ok(());
            }
            state
                .seen
                .retain(|id, _| view.threads.iter().any(|thread| thread.global_id == *id));
            state
                .logpoints
                .retain(|id, _| view.threads.iter().any(|thread| thread.global_id == *id));
            state.ordinary_stops.retain(|id, stop| {
                stop.epoch == epoch
                    && view.threads.iter().any(|thread| {
                        thread.global_id == *id
                            && thread.status == "stopped"
                            && thread.stop_revision == stop.thread.stop_revision
                    })
            });
            state
                .manual_stops
                .retain(|id, _| view.threads.iter().any(|thread| thread.global_id == *id));
            state.automatic_stops.retain(|id, revision| {
                view.threads.iter().any(|thread| {
                    thread.global_id == *id
                        && thread.status == "stopped"
                        && thread.stop_revision == *revision
                })
            });
            // A dispatch callback may have registered a newer native generation
            // after this snapshot was sampled. Keep its receipt; exact revisions
            // prevent it from explaining any other stop, and buckets stay bounded.
            state
                .automatic_pauses
                .retain(|id, _| view.threads.iter().any(|thread| thread.global_id == *id));
            let mut changed = false;
            if state.cause.as_ref().is_some_and(|cause| {
                !view.threads.iter().any(|thread| {
                    thread.global_id == cause.thread.global_id
                        && thread.status == "stopped"
                        && thread.stop_revision == cause.thread.stop_revision
                }) || cause.epoch != epoch
            }) {
                if state
                    .cause
                    .as_ref()
                    .is_some_and(|cause| cause.epoch != epoch)
                {
                    state.public.generation += 1;
                    state.public.target_outcomes.clear();
                }
                state.cause = None;
                state.attempts.clear();
                state.public.initiating_stop = None;
                state.public.pending_targets.clear();
                changed = true;
            }
            for thread in &view.threads {
                if thread.status != "stopped"
                    || (state.seen.get(&thread.global_id) == Some(&thread.stop_revision)
                        && !self
                            .stop_policies
                            .epoch
                            .has_explicit_pause(thread.session_id, epoch))
                {
                    continue;
                }
                let Some(reason) = &thread.stop_reason else {
                    continue;
                };
                if reason
                    .thread_id
                    .is_some_and(|owner| owner != thread.global_id)
                {
                    continue;
                }
                state.seen.insert(thread.global_id, thread.stop_revision);
                let explicit_pause = self
                    .stop_policies
                    .epoch
                    .take_explicit_pause(thread.session_id, epoch);
                if let Some(manual_epoch) = explicit_pause {
                    state.manual_stops.insert(
                        thread.global_id,
                        ManualStop {
                            revision: thread.stop_revision,
                            epoch: manual_epoch,
                        },
                    );
                }
                let manual_epoch = state
                    .manual_stops
                    .get(&thread.global_id)
                    .filter(|stop| stop.revision == thread.stop_revision)
                    .map(|stop| stop.epoch);
                let automatic_pause =
                    state
                        .automatic_pauses
                        .get(&thread.global_id)
                        .is_some_and(|pauses| {
                            pauses.revision == thread.stop_revision
                                && pauses.latest_epoch().is_some_and(|automatic_epoch| {
                                    manual_epoch
                                        .is_none_or(|manual_epoch| manual_epoch < automatic_epoch)
                                })
                        })
                        && (reason.kind == ThreadStopKind::Pause
                            || reason
                                .signal_name
                                .as_deref()
                                .is_some_and(|name| matches!(name, "SIGINT" | "SIGSTOP")));
                if automatic_pause {
                    state
                        .automatic_stops
                        .insert(thread.global_id, thread.stop_revision);
                } else {
                    state.automatic_stops.remove(&thread.global_id);
                }

                // Receipts and manual ownership remain observable after
                // retirement, but an old unseen stop must not start new work.
                let floor = self.stop_policies.epoch.retirement_floor();
                if reason.observed_control_epoch < floor
                    && manual_epoch.is_none_or(|manual_epoch| manual_epoch < floor)
                {
                    continue;
                }

                let target = self.policy_thread_target(thread.global_id)?;
                let Some((_, execution)) = self
                    .projection()
                    .execution_state(target, std::slice::from_ref(thread))?
                else {
                    continue;
                };
                let stop = Stop {
                    thread: thread.clone(),
                    execution,
                    epoch,
                };
                if let Some(message) = reason.log_message.as_ref().filter(|_| {
                    reason.kind == ThreadStopKind::Breakpoint
                        && state
                            .manual_stops
                            .get(&thread.global_id)
                            .map(|stop| stop.revision)
                            != Some(thread.stop_revision)
                }) {
                    let previous = state
                        .logpoints
                        .get(&thread.global_id)
                        .filter(|logpoint| logpoint.revision == thread.stop_revision);
                    if let Some(previous) = previous {
                        if previous.disposition != LogpointDisposition::Failed {
                            continue;
                        }
                    } else {
                        let available = tasks.len() + logpoints.len() < MAX_AUTOMATIC_TASKS;
                        state.logpoints.insert(
                            thread.global_id,
                            LogpointState {
                                revision: thread.stop_revision,
                                epoch,
                                disposition: if available {
                                    LogpointDisposition::InFlight
                                } else {
                                    LogpointDisposition::Failed
                                },
                            },
                        );
                        if available {
                            logpoints.push((stop, message.clone()));
                            continue;
                        }
                        let _ = self.output.publish(
                            Some(thread.session_id),
                            DebuggerOutputStream::Log,
                            "Logpoint left stopped: automatic work limit reached\n",
                        );
                    }
                }
                if !automatic_pause {
                    state.ordinary_stops.insert(thread.global_id, stop);
                }
            }
            // Inspection can advance the public execution revision without
            // changing the retained native stop or its cancellation epoch.
            for thread in &view.threads {
                if let Some(stop) = state.ordinary_stops.get_mut(&thread.global_id) {
                    if let Some((_, execution)) = self.projection().execution_state(
                        self.policy_thread_target(thread.global_id)?,
                        std::slice::from_ref(thread),
                    )? {
                        stop.thread = thread.clone();
                        stop.execution = execution;
                    }
                }
            }
            if let Some(current) = state
                .cause
                .as_ref()
                .and_then(|cause| state.ordinary_stops.get(&cause.thread.global_id).cloned())
            {
                if state.public.initiating_stop.as_ref() != Some(&current.execution) {
                    state.public.initiating_stop = Some(current.execution.clone());
                    changed = true;
                }
                state.cause = Some(current);
            }
            if state.cause.is_none() && state.public.mode != v2::StopMode::Independent as i32 {
                if let Some(stop) = state.ordinary_stops.values().next().cloned() {
                    state.public.generation += 1;
                    // Native turnover preserves deferred stops. Explicit intent
                    // retires their control epoch before another cause can form.
                    state.public.initiating_stop = Some(stop.execution.clone());
                    state.public.target_outcomes.clear();
                    state.cause = Some(stop);
                    state.attempts.clear();
                    changed = true;
                }
            }
            let mut automatic_stops = Vec::new();
            for thread in view.threads.iter().filter(|thread| {
                state.automatic_stops.get(&thread.global_id) == Some(&thread.stop_revision)
            }) {
                if let Some((_, execution)) = self.projection().execution_state(
                    self.policy_thread_target(thread.global_id)?,
                    std::slice::from_ref(thread),
                )? {
                    automatic_stops.push(execution);
                }
            }
            if state.public.automatic_stops != automatic_stops {
                state.public.automatic_stops = automatic_stops;
                changed = true;
            }
            if let Some(cause) = state.cause.clone() {
                for session in &view.sessions {
                    if session.sid == cause.thread.session_id {
                        continue;
                    }
                    if state.public.mode == v2::StopMode::GroupStop as i32
                        && (cause.thread.group_id.is_none()
                            || !view.groups.iter().any(|group| {
                                Some(group.id) == cause.thread.group_id
                                    && group.sids.contains(&session.sid)
                            }))
                    {
                        continue;
                    }
                    let running = view
                        .threads
                        .iter()
                        .filter(|thread| {
                            thread.session_id == session.sid && thread.status == "running"
                        })
                        .map(|thread| (thread.global_id, thread.stop_revision))
                        .collect::<Vec<_>>();
                    if running.is_empty() || state.attempts.get(&session.sid) == Some(&running) {
                        continue;
                    }
                    if tasks.len() + logpoints.len() + pauses.len() >= MAX_AUTOMATIC_TASKS {
                        break;
                    }
                    let target = self.session_target(session.sid)?;
                    if state.public.pending_targets.contains(&target) {
                        continue;
                    }
                    state.attempts.insert(session.sid, running.clone());
                    state.public.pending_targets.push(target.clone());
                    state
                        .public
                        .target_outcomes
                        .retain(|outcome| outcome.target.as_ref() != Some(&target));
                    state.next_attempt += 1;
                    pauses.push((
                        cause.clone(),
                        session.sid,
                        target,
                        state.next_attempt,
                        running,
                    ));
                    changed = true;
                }
            }
            // Scheduling has already reserved these native stops and targets.
            // A bounded journal can reject their projection; it must not leave
            // the reserved work permanently marked seen or pending but unrun.
            publication = if changed {
                self.publish_stop_policy(&mut state)
            } else {
                Ok(())
            };
        }
        for (stop, message) in logpoints {
            let service = self.clone();
            tasks.spawn(async move {
                let result =
                    tokio::time::timeout(AUTOMATION_TIMEOUT, service.run_logpoint(&stop, &message))
                        .await;
                service
                    .logpoint_finished(stop, matches!(result, Ok(Ok(()))))
                    .await;
            });
        }
        for (stop, peer, target, attempt, running) in pauses {
            let service = self.clone();
            tasks.spawn(async move {
                let guard = Arc::new(PeerPauseGuard {
                    model: service.queries.model().clone(),
                    epoch: service.stop_policies.epoch.clone(),
                    stop: stop.clone(),
                    peer,
                    running: running.clone(),
                    attempt,
                    state: service.stop_policies.state.clone(),
                    wake: service.stop_policies.wake.clone(),
                });
                let result = service
                    .automatic_command(
                        &stop,
                        CommandTarget::Session(peer),
                        peer,
                        "-exec-interrupt",
                        guard,
                        &format!("pause-{attempt}"),
                    )
                    .await;
                let mut state = service.stop_policies.state();
                // A failed or lost acknowledgment does not prove the interrupt
                // was unsent. Only a native rejection clears dispatched provenance.
                if state.cause.as_ref().is_none_or(|cause| {
                    cause.thread.global_id != stop.thread.global_id
                        || cause.thread.stop_revision != stop.thread.stop_revision
                        || cause.epoch != stop.epoch
                }) {
                    return;
                }
                state.public.pending_targets.retain(|pending| pending != &target);
                let error = result.err().map(|_| {
                    ApplicationError::new(
                        v2::DdbErrorCode::BackendFailed,
                        "peer pause failed; inspect the automatic execute operation for target details",
                    )
                    .to_contract("stop-policy")
                });
                state.public.target_outcomes.push(v2::TargetOutcome {
                    target: Some(target),
                    succeeded: error.is_none(),
                    error,
                });
                let _ = service.publish_stop_policy(&mut state);
            });
        }
        publication
    }

    fn policy_thread_target(&self, thread: u64) -> Result<v2::Target, ApplicationError> {
        Ok(v2::Target {
            selector: Some(target::Selector::Thread(v2::ThreadTarget {
                thread_id: self.ids.encode(ResourceIdKind::Thread, thread)?,
            })),
        })
    }

    async fn automatic_command(
        self: &Arc<Self>,
        stop: &Stop,
        command_target: CommandTarget,
        session: u64,
        command: &str,
        guard: Arc<dyn DispatchGuard>,
        suffix: &str,
    ) -> anyhow::Result<()> {
        let public = match &command_target {
            CommandTarget::Thread(id) => self.policy_thread_target(id.value())?,
            CommandTarget::Session(id) => self.session_target(*id)?,
            _ => return Err(anyhow!("automatic commands require a concrete target")),
        };
        let active = Arc::new(Mutex::new(true));
        let _lease = AutomationLease(active.clone());
        let guard = Arc::new(ActiveGuard {
            inner: guard,
            active,
        });
        let context = v2::RequestContext {
            idempotency_key: Some(format!(
                "policy-{}-{}-{}-{session}-{suffix}",
                stop.epoch, stop.thread.global_id, stop.thread.stop_revision
            )),
            ..Default::default()
        };
        let scope = RequestScope::begin(Some(&context))?;
        let response = self.admit_command(
            &PrincipalContext::with_scope("ddb-stop-policy", v2::PermissionScope::Control)?,
            &scope,
            Some(&context),
            command.as_bytes(),
            v2::OperationKind::Execute,
            ResolvedTarget {
                public,
                command: command_target,
                session_ids: vec![session],
                resolved_target_count: 1,
            },
            command.into(),
            CompletionProjection::AutomaticExecution(guard),
        )?;
        let operation = self
            .wait_policy_operation(
                &response
                    .operation
                    .context("missing automatic operation")?
                    .operation_id,
            )
            .await?;
        ensure!(
            operation.state == v2::OperationState::Completed as i32,
            "automatic command failed"
        );
        Ok(())
    }

    async fn wait_policy_operation(&self, id: &str) -> anyhow::Result<v2::Operation> {
        tokio::time::timeout(AUTOMATION_TIMEOUT, async {
            loop {
                let operation = self.operations.get(id)?;
                if matches!(
                    v2::OperationState::try_from(operation.state),
                    Ok(v2::OperationState::Completed
                        | v2::OperationState::Failed
                        | v2::OperationState::Cancelled)
                ) {
                    return Ok(operation);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("automatic operation timed out"))?
    }

    async fn run_logpoint(self: &Arc<Self>, stop: &Stop, message: &str) -> anyhow::Result<()> {
        let template = Template::parse(message).map_err(anyhow::Error::msg)?;
        let active = Arc::new(Mutex::new(true));
        let _lease = AutomationLease(active.clone());
        let read_guard: Arc<dyn DispatchGuard> = Arc::new(ActiveGuard {
            inner: Arc::new(LogpointReadGuard {
                model: self.queries.model().clone(),
                epoch: self.stop_policies.epoch.clone(),
                stop: stop.clone(),
            }),
            active,
        });
        read_guard.check().await?;
        let frame = if template
            .parts
            .iter()
            .any(|part| matches!(part, Part::Expression(_)))
        {
            Some(
                self.list_frames_guarded(
                    v2::ListFramesRequest {
                        thread_id: self
                            .ids
                            .encode(ResourceIdKind::Thread, stop.thread.global_id)?,
                        page: Some(v2::PageRequest {
                            page_size: 1,
                            page_token: None,
                        }),
                        ..Default::default()
                    },
                    Some(read_guard.clone()),
                )
                .await?
                .frames
                .first()
                .context("logpoint has no frame")?
                .frame_id
                .clone(),
            )
        } else {
            None
        };
        read_guard.check().await?;
        let mut message = String::new();
        for (index, part) in template.parts.iter().enumerate() {
            read_guard.check().await?;
            let value = match part {
                Part::Text(text) => text.clone(),
                Part::Expression(expression) => {
                    let admission = self
                        .evaluate_guarded(
                            &PrincipalContext::with_scope(
                                "ddb-stop-policy",
                                v2::PermissionScope::Control,
                            )?,
                            v2::EvaluateRequest {
                                context: Some(v2::RequestContext {
                                    idempotency_key: Some(format!(
                                        "log-eval-{}-{}-{}-{index}",
                                        stop.epoch,
                                        stop.thread.global_id,
                                        stop.thread.stop_revision
                                    )),
                                    ..Default::default()
                                }),
                                target: stop.execution.target.clone(),
                                frame_id: frame.clone(),
                                expression: expression.clone(),
                                evaluation_context: v2::EvaluationContext::Watch as i32,
                                ..Default::default()
                            },
                            Some(read_guard.clone()),
                        )
                        .await?;
                    let operation = self
                        .wait_policy_operation(
                            &admission
                                .operation
                                .context("missing evaluation operation")?
                                .operation_id,
                        )
                        .await?;
                    ensure!(
                        operation.state == v2::OperationState::Completed as i32,
                        "logpoint evaluation failed"
                    );
                    match operation.result.and_then(|result| result.value) {
                        Some(operation_result::Value::Evaluation(value)) => value.value,
                        _ => return Err(anyhow!("logpoint evaluation returned no value")),
                    }
                }
            };
            ensure!(
                message.len().saturating_add(value.len()) <= MAX_TEMPLATE_BYTES,
                "logpoint output exceeds 64 KiB"
            );
            message.push_str(&value);
        }
        if !message.ends_with('\n') {
            ensure!(
                message.len() < MAX_TEMPLATE_BYTES,
                "logpoint output exceeds 64 KiB"
            );
            message.push('\n');
        }
        // Public projection revisions can advance during inspection. Backend
        // automation already holds the native stop that its dispatch must claim.
        let guard = Arc::new(super::execution_guard::StopDispatchGuard {
            model: self.queries.model().clone(),
            thread: GlobalThreadId::new(stop.thread.global_id),
            revision: stop.thread.stop_revision,
        });
        let guard = Arc::new(LogpointGuard {
            stop: guard,
            epoch: self.stop_policies.epoch.clone(),
            expected: stop.epoch,
            output: self.output.clone(),
            session: stop.thread.session_id,
            thread: stop.thread.global_id,
            message,
        });
        self.automatic_command(
            stop,
            CommandTarget::Thread(GlobalThreadId::new(stop.thread.global_id)),
            stop.thread.session_id,
            "-record-time-and-continue",
            guard,
            "logpoint",
        )
        .await
    }

    async fn logpoint_finished(&self, stop: Stop, continued: bool) {
        let current = self.queries.thread_by_id(stop.thread.global_id).await;
        let mut state = self.stop_policies.state();
        let Some(logpoint) = state
            .logpoints
            .get_mut(&stop.thread.global_id)
            .filter(|logpoint| {
                logpoint.revision == stop.thread.stop_revision
                    && logpoint.epoch == stop.epoch
                    && logpoint.disposition == LogpointDisposition::InFlight
            })
        else {
            return;
        };
        logpoint.disposition = if continued {
            LogpointDisposition::Continued
        } else {
            LogpointDisposition::Failed
        };
        if !continued
            && self.stop_policies.epoch.current() == stop.epoch
            && current.as_ref().is_some_and(|thread| {
                thread.status == "stopped" && thread.stop_revision == stop.thread.stop_revision
            })
        {
            state.seen.remove(&stop.thread.global_id);
            let _ = self.output.publish(
                Some(stop.thread.session_id),
                DebuggerOutputStream::Log,
                "Logpoint failed; left the initiating thread stopped\n",
            );
        }
        self.stop_policies.wake.notify_one();
    }
}

#[cfg(test)]
mod prerequisite_tests {
    include!("prerequisite_tests.rs");
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    #[tokio::test]
    async fn an_expired_lease_or_native_event_veto_cannot_publish_logpoint_output() {
        for expired in [false, true] {
            let model = RuntimeModel::new();
            model.register_session(1, "logpoint", None).await;
            model.complete_session_activation(1, None).await;
            model.register_thread_group(1, "i1").await.unwrap();
            let thread = model.register_thread(1, 1, "i1").await.unwrap().thread_id;
            model
                .update_thread_statuses(1, &[1], crate::state::ThreadStatus::STOPPED)
                .await
                .unwrap();
            let revision = model.thread_snapshots_for_sessions(&[1]).await[0].stop_revision;
            let output = OutputHub::new(Default::default());
            let mut subscriber = output.subscribe(None).unwrap();
            let active = Arc::new(Mutex::new(true));
            let lease = AutomationLease(active.clone());
            let guard = ActiveGuard {
                active,
                inner: Arc::new(LogpointGuard {
                    stop: Arc::new(super::super::execution_guard::StopDispatchGuard {
                        model: model.clone(),
                        thread,
                        revision,
                    }),
                    epoch: model.execution_control_epoch(),
                    expected: 0,
                    output,
                    session: 1,
                    thread: thread.value(),
                    message: "never emitted\n".into(),
                }),
            };
            let mut permit = guard.prepare().await.unwrap();
            if expired {
                drop(lease);
            }
            let mut admitted = false;
            let result = permit.dispatch(
                &mut || {
                    if expired {
                        Ok(())
                    } else {
                        Err(StaleExecution.into())
                    }
                },
                &mut || {
                    admitted = true;
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert!(!admitted);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), subscriber.recv())
                    .await
                    .is_err()
            );
        }
    }
}
