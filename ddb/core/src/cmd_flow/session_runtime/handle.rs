//! Client surface for one session runtime: tickets, leases, and control.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::SystemTime,
};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use tokio::sync::{
    broadcast, mpsc, oneshot, watch, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock,
};

use crate::{
    cmd_flow::event::DebuggerEventReducer,
    cmd_flow::response::{ParsedSessionResponse, SessionRuntimeStatus},
    common::counter::SimpleCounter,
    connection::RunningTransport,
    debugger::protocol::DebuggerProtocol,
    session::lifecycle::SessionTerminationReporter,
};

use super::{
    actor::{run_session, RuntimeShared, RuntimeWire},
    RuntimeConfig, SessionCommand, COMMAND_MAILBOX_CAPACITY, MAX_PENDING_COMMANDS,
};

/// Safe, detached view of a command admitted to one session runtime.
#[derive(Clone, Debug)]
pub(crate) struct SessionPendingCommand {
    pub(crate) sid: u64,
    pub(crate) token: u64,
    pub(crate) operation_id: Option<String>,
    pub(crate) operation_kind: Option<u32>,
    pub(crate) enqueued_at: SystemTime,
    pub(crate) running: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum PendingCommandChange {
    Upsert(SessionPendingCommand),
    Removed { sid: u64, token: u64 },
    Reconcile,
}

#[derive(Default)]
struct PendingCommandRegistryState {
    entries: HashMap<u64, SessionPendingCommand>,
    events: Option<broadcast::Sender<PendingCommandChange>>,
}

struct PendingCommandRegistry {
    sid: u64,
    state: Mutex<PendingCommandRegistryState>,
}

impl PendingCommandRegistry {
    fn new(sid: u64) -> Arc<Self> {
        Arc::new(Self {
            sid,
            state: Mutex::new(PendingCommandRegistryState::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, PendingCommandRegistryState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn register(
        self: &Arc<Self>,
        token: u64,
        metadata: &crate::cmd_flow::input::CommandMetadata,
    ) -> Result<PendingRegistration> {
        let mut state = self.state();
        if state.entries.len() >= MAX_PENDING_COMMANDS {
            return Err(anyhow!(
                "session {} pending command capacity is exhausted",
                self.sid
            ));
        }
        let command = SessionPendingCommand {
            sid: self.sid,
            token,
            operation_id: metadata.operation_id.clone(),
            operation_kind: metadata.operation_kind,
            enqueued_at: SystemTime::now(),
            running: false,
        };
        state.entries.insert(token, command.clone());
        if let Some(events) = &state.events {
            let _ = events.send(PendingCommandChange::Upsert(command));
        }
        Ok(PendingRegistration {
            token,
            registry: Arc::clone(self),
        })
    }

    fn snapshot(&self) -> Vec<SessionPendingCommand> {
        let mut commands = self.state().entries.values().cloned().collect::<Vec<_>>();
        commands.sort_unstable_by_key(|command| command.token);
        commands
    }

    fn attach(&self, events: broadcast::Sender<PendingCommandChange>) {
        let mut state = self.state();
        state.events = Some(events.clone());
        for command in state.entries.values() {
            let _ = events.send(PendingCommandChange::Upsert(command.clone()));
        }
    }

    fn detach(&self) {
        self.state().events = None;
    }
}

/// RAII registration: cancellation, send failure, timeout, and actor shutdown
/// all remove the projection without a separate cleanup path.
pub(super) struct PendingRegistration {
    token: u64,
    registry: Arc<PendingCommandRegistry>,
}

impl PendingRegistration {
    pub(super) fn mark_running(&self) {
        let mut state = self.registry.state();
        let changed = if let Some(command) = state.entries.get_mut(&self.token) {
            if command.running {
                None
            } else {
                command.running = true;
                Some(command.clone())
            }
        } else {
            None
        };
        if let (Some(command), Some(events)) = (changed, state.events.as_ref()) {
            let _ = events.send(PendingCommandChange::Upsert(command));
        }
    }
}

impl Drop for PendingRegistration {
    fn drop(&mut self) {
        let mut state = self.registry.state();
        if state.entries.remove(&self.token).is_some() {
            if let Some(events) = &state.events {
                let _ = events.send(PendingCommandChange::Removed {
                    sid: self.registry.sid,
                    token: self.token,
                });
            }
        }
    }
}

pub(super) enum CommandPermit {
    Shared {
        _guard: OwnedRwLockReadGuard<()>,
    },
    Exclusive {
        _guard: Arc<OwnedRwLockWriteGuard<()>>,
    },
}

pub(super) enum RuntimeRequest {
    Execute {
        token: u64,
        command: SessionCommand,
        permit: CommandPermit,
        completion: oneshot::Sender<Result<ParsedSessionResponse>>,
        registration: PendingRegistration,
    },
    WriteRaw {
        data: Bytes,
        written: oneshot::Sender<Result<()>>,
    },
}

pub(super) enum ControlRequest {
    Shutdown { stopped: oneshot::Sender<()> },
}

pub struct SessionTicket {
    sid: u64,
    token: u64,
    completion: oneshot::Receiver<Result<ParsedSessionResponse>>,
}

impl SessionTicket {
    pub(crate) fn sid(&self) -> u64 {
        self.sid
    }

    #[cfg(test)]
    pub fn token(&self) -> u64 {
        self.token
    }

    pub async fn complete(self) -> Result<ParsedSessionResponse> {
        self.completion
            .await
            .map_err(|_| anyhow!("session {} dropped command {}", self.sid, self.token))?
    }
}

#[derive(Clone)]
pub struct SessionHandle {
    sid: u64,
    requests: mpsc::Sender<RuntimeRequest>,
    control: mpsc::UnboundedSender<ControlRequest>,
    gate: Arc<RwLock<()>>,
    ready: watch::Receiver<bool>,
    /// Wire correlation tokens for this session, minted once per submission.
    tokens: Arc<SimpleCounter>,
    in_flight: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    pending: Arc<PendingCommandRegistry>,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("sid", &self.sid)
            .field("in_flight", &self.in_flight.load(Ordering::Acquire))
            .field("ready", &*self.ready.borrow())
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl SessionHandle {
    pub(crate) fn spawn(
        sid: u64,
        transport: RunningTransport,
        protocol: Box<dyn DebuggerProtocol>,
        termination: SessionTerminationReporter,
        reducer: Arc<DebuggerEventReducer>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        Self::spawn_with_config(
            sid,
            transport,
            protocol,
            termination,
            reducer,
            RuntimeConfig::default(),
        )
    }

    pub(super) fn spawn_with_config(
        sid: u64,
        transport: RunningTransport,
        protocol: Box<dyn DebuggerProtocol>,
        termination: SessionTerminationReporter,
        reducer: Arc<DebuggerEventReducer>,
        config: RuntimeConfig,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let starts_ready = protocol.starts_ready();
        let (ready_tx, ready) = watch::channel(starts_ready);
        let (requests, request_rx) = mpsc::channel(COMMAND_MAILBOX_CAPACITY);
        let (control, control_rx) = mpsc::unbounded_channel();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let pending = PendingCommandRegistry::new(sid);
        let handle = Self {
            sid,
            requests,
            control,
            gate: Arc::new(RwLock::new(())),
            tokens: Arc::new(SimpleCounter::new()),
            in_flight: Arc::clone(&in_flight),
            closed: Arc::clone(&closed),
            pending,
            ready,
        };
        let task = tokio::spawn(run_session(
            sid,
            request_rx,
            control_rx,
            RuntimeWire {
                transport,
                protocol,
                ready: ready_tx,
            },
            RuntimeShared {
                in_flight,
                closed,
                termination,
            },
            reducer,
            config,
        ));
        (handle, task)
    }

    pub fn sid(&self) -> u64 {
        self.sid
    }

    pub async fn wait_until_ready(&self) -> Result<()> {
        let mut ready = self.ready.clone();
        if *ready.borrow() {
            return Ok(());
        }
        loop {
            ready.changed().await.map_err(|_| {
                anyhow!("session {} stopped before its protocol was ready", self.sid)
            })?;
            if *ready.borrow_and_update() {
                return Ok(());
            }
        }
    }

    pub async fn submit(&self, command: SessionCommand) -> Result<SessionTicket> {
        let permit = CommandPermit::Shared {
            _guard: Arc::clone(&self.gate).read_owned().await,
        };
        self.submit_with_permit(command, permit).await
    }

    pub(crate) async fn execute(&self, command: SessionCommand) -> Result<ParsedSessionResponse> {
        self.submit(command).await?.complete().await
    }

    async fn submit_with_permit(
        &self,
        command: SessionCommand,
        permit: CommandPermit,
    ) -> Result<SessionTicket> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("session {} is closed", self.sid));
        }
        let token = self.tokens.next();
        let registration = self.pending.register(token, &command.metadata)?;
        let (completion, result) = oneshot::channel();
        self.requests
            .send(RuntimeRequest::Execute {
                token,
                command,
                permit,
                completion,
                registration,
            })
            .await
            .map_err(|_| anyhow!("session {} command mailbox is closed", self.sid))?;
        Ok(SessionTicket {
            sid: self.sid,
            token,
            completion: result,
        })
    }

    pub async fn write_raw(&self, data: impl Into<Bytes>) -> Result<()> {
        let (written, result) = oneshot::channel();
        self.requests
            .send(RuntimeRequest::WriteRaw {
                data: data.into(),
                written,
            })
            .await
            .map_err(|_| anyhow!("session {} command mailbox is closed", self.sid))?;
        result
            .await
            .map_err(|_| anyhow!("session {} stopped before writing raw data", self.sid))?
    }

    pub async fn exclusive(&self) -> Result<SessionLease> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("session {} is closed", self.sid));
        }
        let permit = Arc::new(Arc::clone(&self.gate).write_owned().await);
        Ok(SessionLease {
            handle: self.clone(),
            permit,
        })
    }

    pub async fn shutdown(&self) {
        let (stopped, result) = oneshot::channel();
        if self
            .control
            .send(ControlRequest::Shutdown { stopped })
            .is_ok()
        {
            let _ = result.await;
        }
    }

    pub fn status(&self) -> SessionRuntimeStatus {
        SessionRuntimeStatus {
            sid: self.sid,
            in_flight: self.in_flight.load(Ordering::Acquire),
            queued: self.requests.max_capacity() - self.requests.capacity(),
            closed: self.closed.load(Ordering::Acquire),
        }
    }

    pub(crate) fn pending_commands(&self) -> Vec<SessionPendingCommand> {
        self.pending.snapshot()
    }

    pub(crate) fn attach_pending_events(&self, events: broadcast::Sender<PendingCommandChange>) {
        self.pending.attach(events);
    }

    pub(crate) fn detach_pending_events(&self) {
        self.pending.detach();
    }
}

pub struct SessionLease {
    handle: SessionHandle,
    permit: Arc<OwnedRwLockWriteGuard<()>>,
}

impl SessionLease {
    pub fn sid(&self) -> u64 {
        self.handle.sid
    }

    pub async fn submit(&self, command: SessionCommand) -> Result<SessionTicket> {
        self.handle
            .submit_with_permit(
                command,
                CommandPermit::Exclusive {
                    _guard: Arc::clone(&self.permit),
                },
            )
            .await
    }

    pub async fn execute(&self, command: SessionCommand) -> Result<ParsedSessionResponse> {
        self.submit(command).await?.complete().await
    }
}
