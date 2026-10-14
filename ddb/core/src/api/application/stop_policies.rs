//! One backend owns stop classification, logpoint work, and peer pause attempts.
//! Client execution retires pending automation. Every delayed command checks its
//! generation again in the session actor, after preceding events have committed.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
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
        input::{DispatchGuard, StaleExecution},
        logpoint_template::{Part, Template, MAX_TEMPLATE_BYTES},
        output_hub::{DebuggerOutputStream, OutputHub},
        router::Target as CommandTarget,
    },
    state::{GlobalThreadId, RuntimeModel, ThreadStopKind},
};

const MAX_AUTOMATIC_TASKS: usize = 16;
const AUTOMATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct Stop {
    thread: ThreadView,
    execution: v2::ExecutionState,
    epoch: u64,
}

struct PolicyState {
    public: v2::StopPolicy,
    seen: HashMap<u64, u64>,
    logpoints_seen: HashMap<u64, u64>,
    cause: Option<Stop>,
    attempts: BTreeMap<u64, Vec<(u64, u64)>>,
    next_attempt: u64,
    // Native revision and attempt identify only interrupts actually dispatched.
    automatic_pauses: HashMap<u64, (u64, u64)>,
}

pub(super) struct StopPolicies {
    state: Arc<Mutex<PolicyState>>,
    epoch: Arc<AtomicU64>,
    wake: Arc<Notify>,
}

impl StopPolicies {
    #[cfg(test)]
    pub fn classified_stop(&self, thread: u64, revision: u64) -> bool {
        self.state().seen.get(&thread) == Some(&revision)
    }

    pub fn new(epoch: Arc<AtomicU64>) -> Self {
        Self {
            state: Arc::new(Mutex::new(PolicyState {
                public: v2::StopPolicy {
                    mode: v2::StopMode::Independent as i32,
                    revision: 1,
                    ..Default::default()
                },
                seen: HashMap::new(),
                logpoints_seen: HashMap::new(),
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
    active: Arc<AtomicBool>,
}

struct AutomationLease(Arc<AtomicBool>);
impl Drop for AutomationLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl DispatchGuard for ActiveGuard {
    fn is_automatic(&self) -> bool {
        true
    }
    fn dispatched(&self) {
        self.inner.dispatched();
    }
    async fn check(&self) -> anyhow::Result<()> {
        if !self.active.load(Ordering::SeqCst) {
            return Err(StaleExecution.into());
        }
        self.inner.check().await?;
        if !self.active.load(Ordering::SeqCst) {
            return Err(StaleExecution.into());
        }
        Ok(())
    }
}

// This guard checks the initiating stop as well as the policy generation. It
// never consumes the cause because several peer sessions can require a pause.
struct PeerPauseGuard {
    model: Arc<RuntimeModel>,
    epoch: Arc<AtomicU64>,
    stop: Stop,
    peer: u64,
    running: Vec<(u64, u64)>,
    attempt: u64,
    state: Arc<Mutex<PolicyState>>,
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
    async fn check(&self) -> anyhow::Result<()> {
        let threads = self
            .model
            .thread_snapshots_for_sessions(&[self.stop.thread.session_id, self.peer])
            .await;
        let current = threads.iter().any(|thread| {
            thread.global_id.value() == self.stop.thread.global_id
                && thread.status == crate::state::ThreadStatus::STOPPED
                && thread.stop_revision == self.stop.thread.stop_revision
        });
        let running = threads
            .iter()
            .filter(|thread| {
                thread.session_id == self.peer
                    && thread.status == crate::state::ThreadStatus::RUNNING
            })
            .map(|thread| (thread.global_id.value(), thread.stop_revision))
            .collect::<Vec<_>>();
        if !current
            || running != self.running
            || self.epoch.load(Ordering::SeqCst) != self.stop.epoch
        {
            return Err(StaleExecution.into());
        }
        Ok(())
    }
    fn dispatched(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (thread, revision) in &self.running {
            state
                .automatic_pauses
                .insert(*thread, (revision.saturating_add(1), self.attempt));
        }
    }
}

struct LogpointReadGuard {
    model: Arc<RuntimeModel>,
    epoch: Arc<AtomicU64>,
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
    async fn check(&self) -> anyhow::Result<()> {
        let threads = self
            .model
            .thread_snapshots_for_sessions(&[self.stop.thread.session_id])
            .await;
        if self.epoch.load(Ordering::SeqCst) != self.stop.epoch
            || !threads.iter().any(|thread| {
                thread.global_id.value() == self.stop.thread.global_id
                    && thread.status == crate::state::ThreadStatus::STOPPED
                    && thread.stop_revision == self.stop.thread.stop_revision
            })
        {
            return Err(StaleExecution.into());
        }
        Ok(())
    }
}

struct LogpointGuard {
    stop: Arc<dyn DispatchGuard>,
    epoch: Arc<AtomicU64>,
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
    async fn check(&self) -> anyhow::Result<()> {
        if self.epoch.load(Ordering::SeqCst) != self.expected {
            return Err(StaleExecution.into());
        }
        self.stop.check().await?;
        if self.epoch.load(Ordering::SeqCst) != self.expected {
            return Err(StaleExecution.into());
        }
        self.output.publish_for_thread(
            Some(self.session),
            Some(self.thread),
            DebuggerOutputStream::Console,
            self.message.clone(),
        )?;
        Ok(())
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
        let mut state = self.stop_policies.state();
        self.stop_policies.epoch.fetch_add(1, Ordering::SeqCst);
        state.cause = None;
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
        self.stop_policies.epoch.fetch_add(1, Ordering::SeqCst);
        state.cause = None;
        let logpoints_seen = state.logpoints_seen.clone();
        state
            .seen
            .retain(|id, revision| logpoints_seen.get(id) == Some(revision));
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
        let epoch = self.stop_policies.epoch.load(Ordering::SeqCst);
        let view = self.queries.snapshot().await;
        let mut logpoints = Vec::new();
        let mut pauses = Vec::new();
        {
            let mut state = self.stop_policies.state();
            if self.stop_policies.epoch.load(Ordering::SeqCst) != epoch {
                return Ok(());
            }
            state
                .seen
                .retain(|id, _| view.threads.iter().any(|thread| thread.global_id == *id));
            state
                .logpoints_seen
                .retain(|id, _| view.threads.iter().any(|thread| thread.global_id == *id));
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
                    || state.seen.get(&thread.global_id) == Some(&thread.stop_revision)
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
                let automatic_pause = state
                    .automatic_pauses
                    .remove(&thread.global_id)
                    .is_some_and(|(revision, _)| revision == thread.stop_revision)
                    && (reason.kind == ThreadStopKind::Pause
                        || reason
                            .signal_name
                            .as_deref()
                            .is_some_and(|name| matches!(name, "SIGINT" | "SIGSTOP")));

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
                if let Some(message) = reason
                    .log_message
                    .as_ref()
                    .filter(|_| reason.kind == ThreadStopKind::Breakpoint)
                {
                    state
                        .logpoints_seen
                        .insert(thread.global_id, thread.stop_revision);
                    if tasks.len() + logpoints.len() < MAX_AUTOMATIC_TASKS {
                        logpoints.push((stop, message.clone()));
                        continue;
                    }
                    let _ = self.output.publish(
                        Some(thread.session_id),
                        DebuggerOutputStream::Log,
                        "Logpoint left stopped: automatic work limit reached\n",
                    );
                }
                if state.cause.is_none()
                    && state.public.mode != v2::StopMode::Independent as i32
                    && !automatic_pause
                {
                    state.public.generation += 1;
                    // A native stop does not retire another thread's transparent
                    // logpoint. Explicit client intent owns epoch cancellation.
                    state.public.initiating_stop = Some(stop.execution.clone());
                    state.public.target_outcomes.clear();
                    state.cause = Some(stop);
                    state.attempts.clear();
                    changed = true;
                }
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
            if changed {
                self.publish_stop_policy(&mut state)?;
            }
        }
        for (stop, message) in logpoints {
            let service = self.clone();
            tasks.spawn(async move {
                let result =
                    tokio::time::timeout(AUTOMATION_TIMEOUT, service.run_logpoint(&stop, &message))
                        .await;
                if !matches!(result, Ok(Ok(()))) {
                    service.logpoint_failed(stop).await;
                }
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
                if result.is_err() {
                    for (thread, revision) in &running {
                        if state.automatic_pauses.get(thread) == Some(&(revision.saturating_add(1), attempt)) {
                            state.automatic_pauses.remove(thread);
                        }
                    }
                }
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
        Ok(())
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
        let active = Arc::new(AtomicBool::new(true));
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
        let active = Arc::new(AtomicBool::new(true));
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
                self.list_frames(v2::ListFramesRequest {
                    thread_id: self
                        .ids
                        .encode(ResourceIdKind::Thread, stop.thread.global_id)?,
                    page: Some(v2::PageRequest {
                        page_size: 1,
                        page_token: None,
                    }),
                    ..Default::default()
                })
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

    async fn logpoint_failed(&self, stop: Stop) {
        let current = self.queries.thread_by_id(stop.thread.global_id).await;
        if self.stop_policies.epoch.load(Ordering::SeqCst) != stop.epoch
            || current.as_ref().is_none_or(|thread| {
                thread.status != "stopped" || thread.stop_revision != stop.thread.stop_revision
            })
        {
            return;
        }
        let _ = self.output.publish(
            Some(stop.thread.session_id),
            DebuggerOutputStream::Log,
            "Logpoint failed; left the initiating thread stopped\n",
        );
        let mut state = self.stop_policies.state();
        if self.stop_policies.epoch.load(Ordering::SeqCst) != stop.epoch {
            return;
        }
        if state.cause.is_none() && state.public.mode != v2::StopMode::Independent as i32 {
            state.public.generation += 1;
            state.public.initiating_stop = Some(stop.execution.clone());
            state.cause = Some(stop);
            state.attempts.clear();
            let _ = self.publish_stop_policy(&mut state);
        }
        self.stop_policies.wake.notify_one();
    }
}
