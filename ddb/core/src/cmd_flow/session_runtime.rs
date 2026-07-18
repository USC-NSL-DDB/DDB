use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use bytes::{Bytes, BytesMut};
use futures::{stream::FuturesUnordered, StreamExt};
use gdbmi::parser::{Message, Response};
use tokio::sync::{mpsc, oneshot, watch, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tracing::{debug, trace, warn};

use crate::{
    connection::{RunningTransport, TransportEvent},
    dbg_parser::gdb_parser::GdbParser,
};

use super::{
    event::project_event,
    response::{ParsedSessionResponse, SessionRuntimeStatus},
};

const COMMAND_MAILBOX_CAPACITY: usize = 256;
const EVENT_MAILBOX_CAPACITY: usize = 256;
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct RuntimeConfig {
    command_timeout: Duration,
    sweep_interval: Duration,
    #[cfg(test)]
    projector_delay: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            command_timeout: COMMAND_TIMEOUT,
            sweep_interval: COMMAND_SWEEP_INTERVAL,
            #[cfg(test)]
            projector_delay: Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CompletionConsistency {
    ProtocolComplete,
    StateConsistent,
}

impl Default for CompletionConsistency {
    fn default() -> Self {
        Self::StateConsistent
    }
}

#[derive(Debug, Clone)]
pub struct SessionCommand {
    pub token: u64,
    pub command: String,
    pub thread_id: Option<u64>,
    pub consistency: CompletionConsistency,
}

impl SessionCommand {
    fn wire_command(&self) -> String {
        let tracked = if self.command.ends_with('\n') {
            format!("{}{}", self.token, self.command)
        } else {
            format!("{}{}\n", self.token, self.command)
        };
        match self.thread_id {
            Some(thread_id) => format!("-thread-select {}\n{}", thread_id, tracked),
            None => tracked,
        }
    }
}

enum CommandPermit {
    Shared {
        _guard: OwnedRwLockReadGuard<()>,
    },
    Exclusive {
        _guard: Arc<OwnedRwLockWriteGuard<()>>,
    },
}

struct PendingCommand {
    completion: oneshot::Sender<Result<ParsedSessionResponse>>,
    permit: CommandPermit,
    consistency: CompletionConsistency,
    created_at: Instant,
}

enum RuntimeRequest {
    Execute {
        command: SessionCommand,
        permit: CommandPermit,
        completion: oneshot::Sender<Result<ParsedSessionResponse>>,
    },
    WriteRaw {
        data: Bytes,
        written: oneshot::Sender<Result<()>>,
    },
}

enum ControlRequest {
    Shutdown { stopped: oneshot::Sender<()> },
}

enum WriteOwner {
    Command(u64),
    Raw(oneshot::Sender<Result<()>>),
}

type WriteCompletion = Pin<Box<dyn Future<Output = (WriteOwner, Result<()>)> + Send>>;

fn await_write(
    owner: WriteOwner,
    acknowledgement: oneshot::Receiver<Result<()>>,
) -> WriteCompletion {
    Box::pin(async move {
        let result = acknowledgement
            .await
            .map_err(|_| anyhow!("transport stopped before confirming write"))
            .and_then(|result| result);
        (owner, result)
    })
}

struct ProjectedEvent {
    sequence: u64,
    token: Option<gdbmi::Token>,
    message: String,
    payload: gdbmi::raw::Dict,
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
    pub fn spawn(sid: u64, transport: RunningTransport) -> (Self, tokio::task::JoinHandle<()>) {
        Self::spawn_with_config(sid, transport, RuntimeConfig::default())
    }

    fn spawn_with_config(
        sid: u64,
        transport: RunningTransport,
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
            in_flight: Arc::clone(&in_flight),
            closed: Arc::clone(&closed),
        };
        let task = tokio::spawn(run_session(
            sid, request_rx, control_rx, transport, in_flight, closed, config,
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

    async fn submit_with_permit(
        &self,
        command: SessionCommand,
        permit: CommandPermit,
    ) -> Result<SessionTicket> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("session {} is closed", self.sid));
        }
        let token = command.token;
        let (completion, result) = oneshot::channel();
        self.requests
            .send(RuntimeRequest::Execute {
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

async fn run_projector(
    sid: u64,
    mut events: mpsc::Receiver<ProjectedEvent>,
    applied: watch::Sender<u64>,
    #[cfg(test)] projection_delay: Duration,
) {
    while let Some(event) = events.recv().await {
        #[cfg(test)]
        if !projection_delay.is_zero() {
            tokio::time::sleep(projection_delay).await;
        }
        project_event(event.token, event.message, event.payload, sid).await;
        let _ = applied.send(event.sequence);
    }
}

fn fail_pending(pending: &mut HashMap<u64, PendingCommand>, in_flight: &AtomicUsize, reason: &str) {
    for (token, command) in pending.drain() {
        let _ = command
            .completion
            .send(Err(anyhow!("command {} failed: {}", token, reason)));
        in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

fn complete_after_events(
    sid: u64,
    token: u64,
    response: ParsedSessionResponse,
    command: PendingCommand,
    required_sequence: u64,
    mut applied: watch::Receiver<u64>,
    in_flight: Arc<AtomicUsize>,
) {
    tokio::spawn(async move {
        let _permit = command.permit;
        let result = if command.consistency == CompletionConsistency::StateConsistent {
            while *applied.borrow_and_update() < required_sequence {
                if applied.changed().await.is_err() {
                    break;
                }
            }
            if *applied.borrow() < required_sequence {
                Err(anyhow!(
                    "session {} event projector stopped before command {} became state-consistent",
                    sid,
                    token
                ))
            } else {
                Ok(response)
            }
        } else {
            Ok(response)
        };
        let _ = command.completion.send(result);
        in_flight.fetch_sub(1, Ordering::AcqRel);
    });
}

async fn process_stdout(
    sid: u64,
    bytes: Bytes,
    buffer: &mut BytesMut,
    pending: &mut HashMap<u64, PendingCommand>,
    event_sequence: &mut u64,
    event_tx: &mpsc::Sender<ProjectedEvent>,
    applied: &watch::Receiver<u64>,
    in_flight: &Arc<AtomicUsize>,
) -> Result<()> {
    buffer.extend_from_slice(&bytes);
    let Some(last_newline) = buffer.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(());
    };
    let complete = buffer.split_to(last_newline + 1);
    let text = std::str::from_utf8(&complete)?;
    for message in GdbParser::parse_multiple(text) {
        match message {
            Message::Response(Response::Notify {
                token,
                message,
                payload,
            }) => {
                *event_sequence += 1;
                event_tx
                    .send(ProjectedEvent {
                        sequence: *event_sequence,
                        token,
                        message,
                        payload,
                    })
                    .await
                    .map_err(|_| anyhow!("session {} event projector is closed", sid))?;
            }
            Message::Response(Response::Result {
                token: Some(token),
                message,
                payload,
            }) => {
                let token = token.0 as u64;
                if let Some(command) = pending.remove(&token) {
                    complete_after_events(
                        sid,
                        token,
                        ParsedSessionResponse::new(sid, message, payload),
                        command,
                        *event_sequence,
                        applied.clone(),
                        Arc::clone(in_flight),
                    );
                } else {
                    trace!(sid, token, "received result for unknown command");
                }
            }
            Message::Response(Response::Result { token: None, .. }) => {
                trace!(sid, "received tokenless result");
            }
            Message::General(message) => {
                trace!(sid, ?message, "received general debugger output");
            }
        }
    }
    Ok(())
}

async fn run_session(
    sid: u64,
    mut requests: mpsc::Receiver<RuntimeRequest>,
    mut control: mpsc::UnboundedReceiver<ControlRequest>,
    transport: RunningTransport,
    in_flight: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    config: RuntimeConfig,
) {
    let (writer, events) = transport.into_parts();
    let (event_tx, event_rx) = mpsc::channel(EVENT_MAILBOX_CAPACITY);
    let (applied_tx, applied) = watch::channel(0_u64);
    let projector = tokio::spawn(run_projector(
        sid,
        event_rx,
        applied_tx,
        #[cfg(test)]
        config.projector_delay,
    ));
    let mut pending = HashMap::<u64, PendingCommand>::new();
    let mut output = BytesMut::new();
    let mut event_sequence = 0_u64;
    let mut sweeper = tokio::time::interval(config.sweep_interval);
    let mut shutdown_ack = None;
    let mut write_completions = FuturesUnordered::<WriteCompletion>::new();

    'runtime: loop {
        tokio::select! {
            control_request = control.recv() => {
                match control_request {
                    Some(ControlRequest::Shutdown { stopped }) => {
                        shutdown_ack = Some(stopped);
                        break;
                    }
                    None => break,
                }
            }
            request = requests.recv() => {
                match request {
                    Some(RuntimeRequest::Execute { command, permit, completion }) => {
                        let token = command.token;
                        if pending.contains_key(&token) {
                            let _ = completion.send(Err(anyhow!(
                                "session {} already has command token {} in flight",
                                sid,
                                token
                            )));
                            continue;
                        }
                        pending.insert(token, PendingCommand {
                            completion,
                            permit,
                            consistency: command.consistency,
                            created_at: Instant::now(),
                        });
                        in_flight.fetch_add(1, Ordering::AcqRel);
                        let acknowledgement = tokio::select! {
                            control_request = control.recv() => {
                                if let Some(ControlRequest::Shutdown { stopped }) = control_request {
                                    shutdown_ack = Some(stopped);
                                }
                                break 'runtime;
                            }
                            result = writer.start_write(Bytes::from(command.wire_command())) => result,
                        };
                        match acknowledgement {
                            Ok(acknowledgement) => write_completions.push(await_write(
                                WriteOwner::Command(token),
                                acknowledgement,
                            )),
                            Err(error) => {
                                if let Some(command) = pending.remove(&token) {
                                    let _ = command.completion.send(Err(error));
                                    in_flight.fetch_sub(1, Ordering::AcqRel);
                                }
                            }
                        }
                    }
                    Some(RuntimeRequest::WriteRaw { data, written }) => {
                        let acknowledgement = tokio::select! {
                            control_request = control.recv() => {
                                if let Some(ControlRequest::Shutdown { stopped }) = control_request {
                                    shutdown_ack = Some(stopped);
                                }
                                break 'runtime;
                            }
                            result = writer.start_write(data) => result,
                        };
                        match acknowledgement {
                            Ok(acknowledgement) => write_completions
                                .push(await_write(WriteOwner::Raw(written), acknowledgement)),
                            Err(error) => {
                                let _ = written.send(Err(error));
                            }
                        }
                    }
                    None => break,
                }
            }
            Some((owner, result)) = write_completions.next(), if !write_completions.is_empty() => {
                match owner {
                    WriteOwner::Command(token) => {
                        if let Err(error) = result {
                            if let Some(command) = pending.remove(&token) {
                                let _ = command.completion.send(Err(error));
                                in_flight.fetch_sub(1, Ordering::AcqRel);
                            }
                        }
                    }
                    WriteOwner::Raw(written) => {
                        let _ = written.send(result);
                    }
                }
            }
            event = events.recv_async() => {
                match event {
                    Ok(TransportEvent::Stdout(bytes)) => {
                        let result = tokio::select! {
                            control_request = control.recv() => {
                                if let Some(ControlRequest::Shutdown { stopped }) = control_request {
                                    shutdown_ack = Some(stopped);
                                }
                                break 'runtime;
                            }
                            result = process_stdout(
                                sid,
                                bytes,
                                &mut output,
                                &mut pending,
                                &mut event_sequence,
                                &event_tx,
                                &applied,
                                &in_flight,
                            ) => result,
                        };
                        if let Err(error) = result {
                            warn!(sid, ?error, "failed to process debugger output");
                            fail_pending(&mut pending, &in_flight, &error.to_string());
                            break;
                        }
                    }
                    Ok(TransportEvent::Stderr(bytes)) => {
                        debug!(sid, stderr = %String::from_utf8_lossy(&bytes), "debugger stderr");
                    }
                    Ok(TransportEvent::Exited(status)) => {
                        fail_pending(
                            &mut pending,
                            &in_flight,
                            &format!("transport exited with status {:?}", status),
                        );
                        break;
                    }
                    Ok(TransportEvent::Fault(error)) => {
                        fail_pending(&mut pending, &in_flight, &error);
                        break;
                    }
                    Err(_) => {
                        fail_pending(&mut pending, &in_flight, "transport event stream closed");
                        break;
                    }
                }
            }
            _ = sweeper.tick() => {
                let expired = pending
                    .iter()
                    .filter_map(|(token, command)| {
                        (command.created_at.elapsed() >= config.command_timeout).then_some(*token)
                    })
                    .collect::<Vec<_>>();
                for token in expired {
                    if let Some(command) = pending.remove(&token) {
                        let _ = command
                            .completion
                            .send(Err(anyhow!("command {} timed out", token)));
                        in_flight.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            }
        }
    }

    closed.store(true, Ordering::Release);
    fail_pending(&mut pending, &in_flight, "session runtime stopped");
    drop(event_tx);
    if shutdown_ack.is_some() {
        projector.abort();
    }
    let _ = projector.await;
    if let Some(stopped) = shutdown_ack {
        let _ = stopped.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{RunningTransport, TransportEvent, TransportRequest};

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);

    fn test_transport(
        request_capacity: usize,
    ) -> (
        RunningTransport,
        flume::Receiver<TransportRequest>,
        flume::Sender<TransportEvent>,
    ) {
        let (requests, request_rx) = flume::bounded(request_capacity);
        let (event_tx, events) = flume::bounded(32);
        (
            RunningTransport::new(requests, events),
            request_rx,
            event_tx,
        )
    }

    fn command(token: u64) -> SessionCommand {
        SessionCommand {
            token,
            command: "-thread-info".to_string(),
            thread_id: None,
            consistency: CompletionConsistency::ProtocolComplete,
        }
    }

    async fn receive_write(
        requests: &flume::Receiver<TransportRequest>,
    ) -> (String, oneshot::Sender<Result<()>>) {
        let request = tokio::time::timeout(TEST_TIMEOUT, requests.recv_async())
            .await
            .expect("runtime did not submit a transport write")
            .expect("transport request channel closed");
        match request {
            TransportRequest::Write { data, written } => (
                String::from_utf8(data.to_vec()).expect("wire command should be utf-8"),
                written,
            ),
        }
    }

    async fn stop(handle: &SessionHandle, task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(TEST_TIMEOUT, handle.shutdown())
            .await
            .expect("runtime shutdown timed out");
        tokio::time::timeout(TEST_TIMEOUT, task)
            .await
            .expect("runtime task did not stop")
            .expect("runtime task panicked");
    }

    async fn wait_for_no_in_flight(handle: &SessionHandle) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            while handle.status().in_flight != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("in-flight command was not released");
    }

    #[tokio::test]
    async fn pipelines_and_correlates_out_of_order_coalesced_results() {
        let (transport, requests, events) = test_transport(8);
        let (handle, task) = SessionHandle::spawn(41, transport);

        let first = handle.submit(command(1)).await.unwrap();
        let second = handle.submit(command(2)).await.unwrap();
        assert_eq!((first.sid(), first.token()), (41, 1));
        assert_eq!((second.sid(), second.token()), (41, 2));

        let (first_wire, first_ack) = receive_write(&requests).await;
        let (second_wire, second_ack) = receive_write(&requests).await;
        assert_eq!(first_wire, "1-thread-info\n");
        assert_eq!(second_wire, "2-thread-info\n");
        first_ack.send(Ok(())).unwrap();
        second_ack.send(Ok(())).unwrap();

        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(
                b"2^done,value=\"second\"\n1^done,value=\"first\"\n",
            )))
            .await
            .unwrap();

        let first = first.complete().await.unwrap();
        let second = second.complete().await.unwrap();
        assert_eq!(
            first.get_payload().unwrap()["value"]
                .expect_string_ref()
                .unwrap(),
            "first"
        );
        assert_eq!(
            second.get_payload().unwrap()["value"]
                .expect_string_ref()
                .unwrap(),
            "second"
        );
        wait_for_no_in_flight(&handle).await;
        stop(&handle, task).await;
    }

    #[tokio::test]
    async fn buffers_fragmented_protocol_records() {
        let (transport, requests, events) = test_transport(4);
        let (handle, task) = SessionHandle::spawn(42, transport);
        let ticket = handle.submit(command(7)).await.unwrap();
        let (_, acknowledgement) = receive_write(&requests).await;
        acknowledgement.send(Ok(())).unwrap();

        let completion = tokio::spawn(async move { ticket.complete().await });
        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(
                b"7^done,value=\"frag",
            )))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!completion.is_finished());

        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(b"mented\"\n")))
            .await
            .unwrap();
        let response = tokio::time::timeout(TEST_TIMEOUT, completion)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            response.get_payload().unwrap()["value"]
                .expect_string_ref()
                .unwrap(),
            "fragmented"
        );
        stop(&handle, task).await;
    }

    #[tokio::test]
    async fn exclusive_lease_blocks_normal_commands_until_released() {
        let (transport, requests, events) = test_transport(8);
        let (handle, task) = SessionHandle::spawn(43, transport);
        let lease = handle.exclusive().await.unwrap();

        let normal_handle = handle.clone();
        let normal_submit = tokio::spawn(async move { normal_handle.submit(command(12)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(requests.is_empty());

        let exclusive = lease.submit(command(11)).await.unwrap();
        let (wire, acknowledgement) = receive_write(&requests).await;
        assert_eq!(wire, "11-thread-info\n");
        acknowledgement.send(Ok(())).unwrap();
        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(b"11^done\n")))
            .await
            .unwrap();
        exclusive.complete().await.unwrap();

        drop(lease);
        let normal = tokio::time::timeout(TEST_TIMEOUT, normal_submit)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let (wire, acknowledgement) = receive_write(&requests).await;
        assert_eq!(wire, "12-thread-info\n");
        acknowledgement.send(Ok(())).unwrap();
        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(b"12^done\n")))
            .await
            .unwrap();
        normal.complete().await.unwrap();
        stop(&handle, task).await;
    }

    #[tokio::test]
    async fn transport_fault_fails_pending_and_closes_runtime() {
        let (transport, requests, events) = test_transport(4);
        let (handle, task) = SessionHandle::spawn(44, transport);
        let ticket = handle.submit(command(21)).await.unwrap();
        let (_, acknowledgement) = receive_write(&requests).await;
        acknowledgement.send(Ok(())).unwrap();

        events
            .send_async(TransportEvent::Fault(
                "injected transport fault".to_string(),
            ))
            .await
            .unwrap();
        let error = ticket.complete().await.unwrap_err().to_string();
        assert!(error.contains("injected transport fault"));
        tokio::time::timeout(TEST_TIMEOUT, task)
            .await
            .unwrap()
            .unwrap();
        assert!(handle.status().closed);
        assert_eq!(handle.status().in_flight, 0);
    }

    #[tokio::test]
    async fn timeout_and_cancelled_receiver_release_transaction_permits() {
        let (transport, requests, events) = test_transport(8);
        let config = RuntimeConfig {
            command_timeout: Duration::from_millis(25),
            sweep_interval: Duration::from_millis(5),
            projector_delay: Duration::ZERO,
        };
        let (handle, task) = SessionHandle::spawn_with_config(45, transport, config);

        let timed_out = handle.submit(command(31)).await.unwrap();
        let (_, acknowledgement) = receive_write(&requests).await;
        acknowledgement.send(Ok(())).unwrap();
        let error = tokio::time::timeout(TEST_TIMEOUT, timed_out.complete())
            .await
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(error.contains("timed out"));
        wait_for_no_in_flight(&handle).await;

        let cancelled = handle.submit(command(32)).await.unwrap();
        let (_, acknowledgement) = receive_write(&requests).await;
        acknowledgement.send(Ok(())).unwrap();
        drop(cancelled);
        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(b"32^done\n")))
            .await
            .unwrap();
        wait_for_no_in_flight(&handle).await;

        let lease = tokio::time::timeout(TEST_TIMEOUT, handle.exclusive())
            .await
            .expect("released permits should allow an exclusive lease")
            .unwrap();
        drop(lease);
        stop(&handle, task).await;
    }

    #[tokio::test]
    async fn shutdown_interrupts_transport_backpressure() {
        let (transport, _requests, _events) = test_transport(1);
        let (handle, task) = SessionHandle::spawn(46, transport);

        let first = handle.submit(command(41)).await.unwrap();
        let second = handle.submit(command(42)).await.unwrap();
        tokio::time::timeout(TEST_TIMEOUT, handle.shutdown())
            .await
            .expect("control lane should bypass a blocked transport write");
        tokio::time::timeout(TEST_TIMEOUT, task)
            .await
            .expect("runtime should stop under transport backpressure")
            .unwrap();
        assert!(first.complete().await.is_err());
        assert!(second.complete().await.is_err());
    }

    #[tokio::test]
    async fn shutdown_interrupts_projector_backpressure() {
        let (transport, _requests, events) = test_transport(1);
        let config = RuntimeConfig {
            command_timeout: TEST_TIMEOUT,
            sweep_interval: Duration::from_millis(10),
            projector_delay: Duration::from_secs(5),
        };
        let (handle, task) = SessionHandle::spawn_with_config(48, transport, config);
        let notifications = (0..EVENT_MAILBOX_CAPACITY + 8)
            .map(|id| format!("=breakpoint-modified,id=\"{}\"\n", id))
            .collect::<String>();
        events
            .send_async(TransportEvent::Stdout(Bytes::from(notifications)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        tokio::time::timeout(TEST_TIMEOUT, handle.shutdown())
            .await
            .expect("control lane should bypass a saturated projector queue");
        tokio::time::timeout(TEST_TIMEOUT, task)
            .await
            .expect("runtime should stop under projector backpressure")
            .unwrap();
    }

    #[tokio::test]
    async fn completion_mode_controls_state_projection_watermark() {
        let (transport, requests, events) = test_transport(4);
        let config = RuntimeConfig {
            command_timeout: TEST_TIMEOUT,
            sweep_interval: Duration::from_millis(10),
            projector_delay: Duration::from_millis(100),
        };
        let (handle, task) = SessionHandle::spawn_with_config(47, transport, config);

        let mut state_consistent = command(51);
        state_consistent.consistency = CompletionConsistency::StateConsistent;
        let ticket = handle.submit(state_consistent).await.unwrap();
        let (_, acknowledgement) = receive_write(&requests).await;
        acknowledgement.send(Ok(())).unwrap();
        let completion = tokio::spawn(async move { ticket.complete().await });
        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(
                b"=breakpoint-modified,id=\"1\"\n51^done\n",
            )))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!completion.is_finished());
        tokio::time::timeout(TEST_TIMEOUT, completion)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let protocol_complete = handle.submit(command(52)).await.unwrap();
        let (_, acknowledgement) = receive_write(&requests).await;
        acknowledgement.send(Ok(())).unwrap();
        events
            .send_async(TransportEvent::Stdout(Bytes::from_static(
                b"=breakpoint-modified,id=\"2\"\n52^done\n",
            )))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(50), protocol_complete.complete())
            .await
            .expect("protocol-complete response should not wait for projection")
            .unwrap();

        stop(&handle, task).await;
    }
}
