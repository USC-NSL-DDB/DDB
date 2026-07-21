//! Client surface for one session runtime: tickets, leases, and control.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::{
    cmd_flow::event::DebuggerEventReducer,
    cmd_flow::response::{ParsedSessionResponse, SessionRuntimeStatus},
    common::counter::SimpleCounter,
    connection::RunningTransport,
    session::lifecycle::SessionTerminationReporter,
};

use super::{
    actor::{run_session, RuntimeShared},
    RuntimeConfig, SessionCommand, COMMAND_MAILBOX_CAPACITY,
};

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
    #[cfg(test)]
    pub fn sid(&self) -> u64 {
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
    /// Wire correlation tokens for this session, minted once per submission.
    tokens: Arc<SimpleCounter>,
    in_flight: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHandle")
            .field("sid", &self.sid)
            .field("in_flight", &self.in_flight.load(Ordering::Acquire))
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish()
    }
}

impl SessionHandle {
    pub(crate) fn spawn(
        sid: u64,
        transport: RunningTransport,
        termination: SessionTerminationReporter,
        reducer: Arc<DebuggerEventReducer>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        Self::spawn_with_config(
            sid,
            transport,
            termination,
            reducer,
            RuntimeConfig::default(),
        )
    }

    pub(super) fn spawn_with_config(
        sid: u64,
        transport: RunningTransport,
        termination: SessionTerminationReporter,
        reducer: Arc<DebuggerEventReducer>,
        config: RuntimeConfig,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (requests, request_rx) = mpsc::channel(COMMAND_MAILBOX_CAPACITY);
        let (control, control_rx) = mpsc::unbounded_channel();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let handle = Self {
            sid,
            requests,
            control,
            gate: Arc::new(RwLock::new(())),
            tokens: Arc::new(SimpleCounter::new()),
            in_flight: Arc::clone(&in_flight),
            closed: Arc::clone(&closed),
        };
        let task = tokio::spawn(run_session(
            sid,
            request_rx,
            control_rx,
            transport,
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
        let (completion, result) = oneshot::channel();
        self.requests
            .send(RuntimeRequest::Execute {
                token,
                command,
                permit,
                completion,
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
