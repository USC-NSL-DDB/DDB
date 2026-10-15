//! Behavioral tests for the session runtime actor.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, oneshot};

use super::*;
use crate::cmd_flow::event::DebuggerEventReducer;

use crate::session::lifecycle::{self, SessionTermination, SessionTerminationCause};
use crate::{
    cmd_flow::breakpoint::BreakpointEventPublisher,
    connection::{RunningTransport, TransportEvent, TransportRequest},
    debugger::gdb::protocol::GdbMiProtocol,
    debugger::lldb::protocol::LldbJsonProtocol,
    notification::NotificationManager,
    state::RuntimeModel,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct RejectStop;

#[async_trait::async_trait]
impl crate::cmd_flow::input::DispatchGuard for RejectStop {
    async fn check(&self) -> Result<()> {
        Err(crate::cmd_flow::input::StaleExecution.into())
    }
}

struct StopClaimGuard {
    model: Arc<RuntimeModel>,
    thread: crate::state::GlobalThreadId,
    revision: u64,
}

impl std::fmt::Debug for StopClaimGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StopClaimGuard").finish()
    }
}

#[async_trait::async_trait]
impl crate::cmd_flow::input::DispatchGuard for StopClaimGuard {
    async fn check(&self) -> Result<()> {
        if !self
            .model
            .claim_thread_stop(self.thread, self.revision)
            .await
        {
            return Err(crate::cmd_flow::input::StaleExecution.into());
        }
        Ok(())
    }
}

#[derive(Debug)]
struct BlockingGuard {
    inner: Arc<dyn crate::cmd_flow::input::DispatchGuard>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl crate::cmd_flow::input::DispatchGuard for BlockingGuard {
    async fn check(&self) -> Result<()> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.check().await
    }
}

#[derive(Debug, Default)]
struct CountingGuard(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl crate::cmd_flow::input::DispatchGuard for CountingGuard {
    async fn check(&self) -> Result<()> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

const NEW_STOP: &[u8] =
    b"*stopped,reason=\"end-stepping-range\",thread-id=\"1\",stopped-threads=\"all\"\n";

#[derive(Debug)]
struct StopDuringEncoding {
    protocol: GdbMiProtocol,
    events: flume::Sender<TransportEvent>,
}

impl crate::debugger::protocol::DebuggerProtocol for StopDuringEncoding {
    fn encode_command(
        &self,
        command: crate::debugger::protocol::ProtocolCommand<'_>,
    ) -> Result<Bytes> {
        self.events
            .try_send(TransportEvent::Stdout(Bytes::from_static(NEW_STOP)))?;
        self.protocol.encode_command(command)
    }

    fn push_stdout(
        &mut self,
        bytes: Bytes,
    ) -> Result<Vec<crate::debugger::protocol::ProtocolRecord>> {
        self.protocol.push_stdout(bytes)
    }
}

async fn stopped_guard_fixture(sid: u64) -> (Arc<StopClaimGuard>, Arc<DebuggerEventReducer>) {
    let model = RuntimeModel::new();
    model.register_session(sid, "fixture", None).await;
    model.register_thread_group(sid, "i1").await.unwrap();
    let thread = model.register_thread(sid, 1, "i1").await.unwrap().thread_id;
    model
        .update_thread_statuses(sid, &[1], crate::state::ThreadStatus::STOPPED)
        .await
        .unwrap();
    let revision = model.thread_snapshots_for_sessions(&[sid]).await[0].stop_revision;
    let reducer = DebuggerEventReducer::new(
        model.clone(),
        BreakpointEventPublisher::new(
            Arc::new(NotificationManager::new()),
            crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
            crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
        ),
    );
    (
        Arc::new(StopClaimGuard {
            model,
            thread,
            revision,
        }),
        reducer,
    )
}

#[tokio::test]
async fn guarded_dispatch_applies_queued_native_stop_before_claiming() {
    let sid = 54;
    let (guard, reducer) = stopped_guard_fixture(sid).await;
    let (transport, requests, events) = test_transport(8);
    let (lifecycle, _terminations) = lifecycle::channel();
    let (handle, task) = SessionHandle::spawn(
        sid,
        transport,
        Box::new(StopDuringEncoding {
            protocol: GdbMiProtocol::default(),
            events,
        }),
        lifecycle.bind(sid),
        reducer,
    );
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(guard);
    let ticket = handle.submit(guarded).await.unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, ticket.complete())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.is::<crate::cmd_flow::input::StaleExecution>());
    assert!(
        requests.is_empty(),
        "a queued newer stop must prevent dispatch"
    );
    stop(&handle, task).await;
}

#[tokio::test]
async fn guarded_dispatch_rejects_native_events_arriving_during_validation() {
    let sid = 55;
    let (inner, reducer) = stopped_guard_fixture(sid).await;
    let guard = Arc::new(BlockingGuard {
        inner,
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let (transport, requests, events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime_with_reducer(sid, transport, reducer);
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(guard.clone());
    let ticket = handle.submit(guarded).await.unwrap();
    tokio::time::timeout(TEST_TIMEOUT, guard.entered.notified())
        .await
        .unwrap();
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(NEW_STOP)))
        .await
        .unwrap();
    guard.release.notify_one();
    let error = tokio::time::timeout(TEST_TIMEOUT, ticket.complete())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.is::<crate::cmd_flow::input::StaleExecution>());
    assert!(
        requests.is_empty(),
        "an awaited guard must not hide a newer stop"
    );
    stop(&handle, task).await;
}

#[tokio::test(start_paused = true)]
async fn guarded_dispatch_waits_for_prior_writes_before_validating() {
    let (transport, requests, events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime(56, transport);
    let first = handle.submit(command()).await.unwrap();
    let (_, first_ack) = receive_write(&requests).await;
    let guard = Arc::new(CountingGuard::default());
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(guard.clone());
    let ticket = handle.submit(guarded).await.unwrap();
    let mut completion = Box::pin(ticket.complete());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut completion)
            .await
            .is_err()
    );
    assert_eq!(guard.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(
        requests.is_empty(),
        "stop validation must wait for the writer"
    );

    first_ack.send(Ok(())).unwrap();
    let (_, second_ack) = receive_write(&requests).await;
    assert_eq!(guard.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    second_ack.send(Ok(())).unwrap();
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"1^done\n2^done\n",
        )))
        .await
        .unwrap();
    assert!(completion.await.is_ok());
    assert!(first.complete().await.is_ok());
    stop(&handle, task).await;
}

#[tokio::test(start_paused = true)]
async fn guarded_dispatch_revalidates_stops_after_waiting_for_prior_writes() {
    let sid = 70;
    let (guard, reducer) = stopped_guard_fixture(sid).await;
    let (transport, requests, events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime_with_reducer(sid, transport, reducer);
    let first = handle.submit(command()).await.unwrap();
    let (_, first_ack) = receive_write(&requests).await;
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(guard);
    let ticket = handle.submit(guarded).await.unwrap();
    let mut completion = Box::pin(ticket.complete());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut completion)
            .await
            .is_err()
    );
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(NEW_STOP)))
        .await
        .unwrap();
    first_ack.send(Ok(())).unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, completion)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.is::<crate::cmd_flow::input::StaleExecution>());
    assert!(
        requests.is_empty(),
        "a newer stop must veto the waiting command"
    );
    stop(&handle, task).await;
    assert!(first.complete().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn guarded_dispatch_waiting_for_prior_writes_honors_native_deadlines() {
    let (transport, requests, _events) = test_transport(8);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::ZERO,
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(71, transport, config);
    let first = handle.submit(command()).await.unwrap();
    let (_, first_ack) = receive_write(&requests).await;
    let guard = Arc::new(CountingGuard::default());
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(guard.clone());
    let ticket = handle.submit(guarded).await.unwrap();
    let error = tokio::time::timeout(Duration::from_millis(100), ticket.complete())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    first_ack.send(Ok(())).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(guard.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(
        requests.is_empty(),
        "expired work must not be dispatched later"
    );
    stop(&handle, task).await;
    assert!(first.complete().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn guarded_dispatch_waiting_for_prior_writes_is_interrupted_by_shutdown() {
    let (transport, requests, _events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime(72, transport);
    let first = handle.submit(command()).await.unwrap();
    let (_, first_ack) = receive_write(&requests).await;
    let guard = Arc::new(CountingGuard::default());
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(guard.clone());
    let ticket = handle.submit(guarded).await.unwrap();
    let mut completion = Box::pin(ticket.complete());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut completion)
            .await
            .is_err()
    );
    stop(&handle, task).await;
    let _ = first_ack.send(Ok(()));
    assert!(completion.await.is_err());
    assert!(first.complete().await.is_err());
    assert_eq!(guard.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(requests.is_empty());
}

#[tokio::test]
async fn stale_execution_guard_never_reaches_the_transport() {
    let (transport, requests, _events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime(53, transport);
    let mut guarded = command();
    guarded.metadata.dispatch_guard = Some(Arc::new(RejectStop));
    let ticket = handle.submit(guarded).await.unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, ticket.complete())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.is::<crate::cmd_flow::input::StaleExecution>());
    assert!(requests.try_recv().is_err());
    wait_for_no_in_flight(&handle).await;
    stop(&handle, task).await;
}

fn test_reducer() -> Arc<DebuggerEventReducer> {
    test_reducer_with_notifications(Arc::new(NotificationManager::new()))
}

fn test_reducer_with_notifications(
    notifications: Arc<NotificationManager>,
) -> Arc<DebuggerEventReducer> {
    test_reducer_with_sinks(
        notifications,
        crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
    )
}

fn test_reducer_with_sinks(
    notifications: Arc<NotificationManager>,
    output: Arc<crate::cmd_flow::output_hub::OutputHub>,
) -> Arc<DebuggerEventReducer> {
    DebuggerEventReducer::new(
        RuntimeModel::new(),
        BreakpointEventPublisher::new(
            notifications,
            crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
            output,
        ),
    )
}

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

fn command() -> SessionCommand {
    SessionCommand {
        command: "-thread-info".to_string(),
        thread_id: None,
        consistency: CompletionConsistency::ProtocolComplete,
        metadata: Default::default(),
    }
}

fn spawn_runtime(
    sid: u64,
    transport: RunningTransport,
) -> (
    SessionHandle,
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<SessionTermination>,
) {
    let (lifecycle, terminations) = lifecycle::channel();
    let (handle, task) = SessionHandle::spawn(
        sid,
        transport,
        Box::new(GdbMiProtocol::default()),
        lifecycle.bind(sid),
        test_reducer(),
    );
    (handle, task, terminations)
}

fn spawn_runtime_with_reducer(
    sid: u64,
    transport: RunningTransport,
    reducer: Arc<DebuggerEventReducer>,
) -> (
    SessionHandle,
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<SessionTermination>,
) {
    let (lifecycle, terminations) = lifecycle::channel();
    let (handle, task) = SessionHandle::spawn(
        sid,
        transport,
        Box::new(GdbMiProtocol::default()),
        lifecycle.bind(sid),
        reducer,
    );
    (handle, task, terminations)
}

fn spawn_runtime_with_config(
    sid: u64,
    transport: RunningTransport,
    config: RuntimeConfig,
) -> (
    SessionHandle,
    tokio::task::JoinHandle<()>,
    mpsc::UnboundedReceiver<SessionTermination>,
) {
    let (lifecycle, terminations) = lifecycle::channel();
    let (handle, task) = SessionHandle::spawn_with_config(
        sid,
        transport,
        Box::new(GdbMiProtocol::default()),
        lifecycle.bind(sid),
        test_reducer(),
        config,
    );
    (handle, task, terminations)
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

async fn wait_for_pending_running(handle: &SessionHandle, token: u64) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            if handle
                .pending_commands()
                .iter()
                .any(|command| command.token == token && command.running)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending command did not begin running");
}

#[tokio::test]
async fn pending_projection_tracks_queue_running_metadata_and_cleanup() {
    // A one-item transport mailbox lets the actor begin two commands while a
    // third remains queued in its command mailbox deterministically.
    let (transport, _requests, _events) = test_transport(1);
    let (handle, task, _terminations) = spawn_runtime(53, transport);
    let (pending_events, mut pending_changes) = broadcast::channel(32);
    handle.attach_pending_events(pending_events);

    let first = handle.submit(command()).await.unwrap();
    wait_for_pending_running(&handle, 1).await;
    let second = handle.submit(command()).await.unwrap();
    wait_for_pending_running(&handle, 2).await;

    let mut tracked = command();
    tracked.metadata.operation_id = Some("op_test".to_string());
    tracked.metadata.operation_kind = Some(7);
    let third = handle.submit(tracked).await.unwrap();
    let pending = handle.pending_commands();
    assert_eq!(pending.len(), 3);
    let queued = pending
        .iter()
        .find(|command| command.token == 3)
        .expect("third command should be projected");
    assert!(!queued.running);
    assert_eq!(queued.sid, 53);
    assert_eq!(queued.operation_id.as_deref(), Some("op_test"));
    assert_eq!(queued.operation_kind, Some(7));

    tokio::time::timeout(TEST_TIMEOUT, handle.shutdown())
        .await
        .expect("shutdown should interrupt transport backpressure");
    task.await.unwrap();
    assert!(first.complete().await.is_err());
    assert!(second.complete().await.is_err());
    assert!(third.complete().await.is_err());
    assert!(handle.pending_commands().is_empty());

    let mut changes = Vec::new();
    while let Ok(change) = pending_changes.try_recv() {
        changes.push(change);
    }
    for token in [1, 2] {
        assert!(changes.iter().any(|change| matches!(
            change,
            PendingCommandChange::Upsert(command)
                if command.token == token && !command.running
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            PendingCommandChange::Upsert(command)
                if command.token == token && command.running
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            PendingCommandChange::Removed { sid: 53, token: removed }
                if *removed == token
        )));
    }
    assert!(changes.iter().any(|change| matches!(
        change,
        PendingCommandChange::Upsert(command)
            if command.token == 3
                && !command.running
                && command.operation_id.as_deref() == Some("op_test")
    )));
    assert!(!changes.iter().any(|change| matches!(
        change,
        PendingCommandChange::Upsert(command) if command.token == 3 && command.running
    )));
    assert!(changes
        .iter()
        .any(|change| matches!(change, PendingCommandChange::Removed { sid: 53, token: 3 })));
}

#[tokio::test]
async fn pipelines_and_correlates_out_of_order_coalesced_results() {
    let (transport, requests, events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime(41, transport);

    let first = handle.submit(command()).await.unwrap();
    let second = handle.submit(command()).await.unwrap();
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
    let (handle, task, _terminations) = spawn_runtime(42, transport);
    let ticket = handle.submit(command()).await.unwrap();
    let (_, acknowledgement) = receive_write(&requests).await;
    acknowledgement.send(Ok(())).unwrap();

    let completion = tokio::spawn(async move { ticket.complete().await });
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"1^done,value=\"frag",
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
async fn debugger_stream_records_reach_structured_event_subscribers() {
    let (transport, _requests, events) = test_transport(4);
    let notifications = Arc::new(NotificationManager::new());
    let mut subscription = notifications.subscribe().unwrap();
    let output = crate::cmd_flow::output_hub::OutputHub::new(Default::default());
    let mut typed_output = output.subscribe(None).unwrap();
    let reducer = test_reducer_with_sinks(notifications, output);
    let (handle, task, _terminations) = spawn_runtime_with_reducer(52, transport, reducer);

    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"~\"hello from the inferior\\n\"\n",
        )))
        .await
        .unwrap();

    let message = tokio::time::timeout(TEST_TIMEOUT, subscription.recv())
        .await
        .expect("debugger stream did not reach the event subscriber")
        .expect("event subscriber closed unexpectedly");
    let axum::extract::ws::Message::Text(text) = message else {
        panic!("expected a text notification")
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["payload"]["type"], "DebuggerOutput");
    assert_eq!(value["payload"]["data"]["records"][0]["stream"], "console");
    assert_eq!(value["payload"]["data"]["records"][0]["event"], "output");
    assert_eq!(
        value["payload"]["data"]["records"][0]["payload"]["message"],
        "hello from the inferior\n"
    );
    let crate::cmd_flow::output_hub::OutputDelivery::Record(record) =
        tokio::time::timeout(TEST_TIMEOUT, typed_output.recv())
            .await
            .expect("typed output did not arrive")
            .expect("typed output hub closed unexpectedly")
    else {
        panic!("live debugger output must not be represented as a gap")
    };
    assert_eq!(record.session_id, Some(52));
    assert_eq!(
        record.stream,
        crate::cmd_flow::output_hub::DebuggerOutputStream::Console
    );
    assert_eq!(record.text, "hello from the inferior\n");

    events
        .send_async(TransportEvent::Stderr(Bytes::from_static(
            b"debugger diagnostic\n",
        )))
        .await
        .unwrap();
    let message = tokio::time::timeout(TEST_TIMEOUT, subscription.recv())
        .await
        .expect("debugger stderr did not reach the event subscriber")
        .expect("event subscriber closed unexpectedly");
    let axum::extract::ws::Message::Text(text) = message else {
        panic!("expected a text notification")
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["payload"]["data"]["records"][0]["stream"], "log");
    assert_eq!(
        value["payload"]["data"]["records"][0]["payload"]["message"],
        "debugger diagnostic\n"
    );
    let crate::cmd_flow::output_hub::OutputDelivery::Record(record) =
        tokio::time::timeout(TEST_TIMEOUT, typed_output.recv())
            .await
            .expect("typed stderr output did not arrive")
            .expect("typed output hub closed unexpectedly")
    else {
        panic!("live debugger stderr must not be represented as a gap")
    };
    assert_eq!(record.session_id, Some(52));
    assert_eq!(
        record.stream,
        crate::cmd_flow::output_hub::DebuggerOutputStream::Log
    );
    assert_eq!(record.text, "debugger diagnostic\n");

    stop(&handle, task).await;
}

#[tokio::test]
async fn exclusive_lease_blocks_normal_commands_until_released() {
    let (transport, requests, events) = test_transport(8);
    let (handle, task, _terminations) = spawn_runtime(43, transport);
    let lease = handle.exclusive().await.unwrap();

    let normal_handle = handle.clone();
    let normal_submit = tokio::spawn(async move { normal_handle.submit(command()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(requests.is_empty());

    let exclusive = lease.submit(command()).await.unwrap();
    let (wire, acknowledgement) = receive_write(&requests).await;
    assert_eq!(wire, "1-thread-info\n");
    acknowledgement.send(Ok(())).unwrap();
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(b"1^done\n")))
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
    assert_eq!(wire, "2-thread-info\n");
    acknowledgement.send(Ok(())).unwrap();
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(b"2^done\n")))
        .await
        .unwrap();
    normal.complete().await.unwrap();
    stop(&handle, task).await;
}

#[tokio::test]
async fn transport_fault_fails_pending_and_closes_runtime() {
    let (transport, requests, events) = test_transport(4);
    let (handle, task, mut terminations) = spawn_runtime(44, transport);
    let ticket = handle.submit(command()).await.unwrap();
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
    assert_eq!(
        terminations.recv().await.unwrap(),
        SessionTermination {
            sid: 44,
            cause: SessionTerminationCause::TransportFault {
                message: "injected transport fault".into(),
            },
        }
    );
}

#[tokio::test]
async fn protocol_exit_requests_supervised_termination() {
    let (transport, _requests, events) = test_transport(2);
    let (handle, task, mut terminations) = spawn_runtime(50, transport);

    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"*stopped,reason=\"exited-normally\"\n",
        )))
        .await
        .unwrap();

    assert_eq!(
        tokio::time::timeout(TEST_TIMEOUT, terminations.recv())
            .await
            .expect("protocol exit should reach lifecycle supervision")
            .unwrap(),
        SessionTermination {
            sid: 50,
            cause: SessionTerminationCause::ProtocolExit {
                reasons: vec!["exited-normally".into()],
            },
        }
    );

    stop(&handle, task).await;
}

#[tokio::test]
async fn timeout_and_cancelled_receiver_release_transaction_permits() {
    let (transport, requests, events) = test_transport(8);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::ZERO,
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(45, transport, config);

    let timed_out = handle.submit(command()).await.unwrap();
    let (_, acknowledgement) = receive_write(&requests).await;
    acknowledgement.send(Ok(())).unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, timed_out.complete())
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("timed out"));
    wait_for_no_in_flight(&handle).await;

    let cancelled = handle.submit(command()).await.unwrap();
    let (_, acknowledgement) = receive_write(&requests).await;
    acknowledgement.send(Ok(())).unwrap();
    drop(cancelled);
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(b"2^done\n")))
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

struct ProjectionWaitFixture {
    handle: SessionHandle,
    task: tokio::task::JoinHandle<()>,
    ticket: SessionTicket,
    _events: flume::Sender<TransportEvent>,
}

impl ProjectionWaitFixture {
    async fn new(command_timeout: Duration, projector_delay: Duration) -> Self {
        let (transport, requests, events) = test_transport(4);
        let config = RuntimeConfig {
            command_timeout,
            sweep_interval: Duration::from_millis(5),
            projector_delay,
            publisher_delay: Duration::ZERO,
        };
        let (handle, task, _terminations) = spawn_runtime_with_config(63, transport, config);
        let mut state_consistent = command();
        state_consistent.consistency = CompletionConsistency::StateConsistent;
        let ticket = handle.submit(state_consistent).await.unwrap();
        let (_, written) = receive_write(&requests).await;
        written.send(Ok(())).unwrap();

        // A second result in the same stdout chunk proves the actor transferred
        // the first command into its projection barrier before the test proceeds.
        let follower = handle.submit(command()).await.unwrap();
        let (_, written) = receive_write(&requests).await;
        written.send(Ok(())).unwrap();
        events
            .send_async(TransportEvent::Stdout(Bytes::from(format!(
                "=breakpoint-modified,id=\"1\"\n{}^done\n{}^done\n",
                ticket.token(),
                follower.token()
            ))))
            .await
            .unwrap();
        tokio::time::timeout(TEST_TIMEOUT, follower.complete())
            .await
            .unwrap()
            .unwrap();
        Self {
            handle,
            task,
            ticket,
            _events: events,
        }
    }
}

#[tokio::test(start_paused = true)]
async fn native_command_deadline_bounds_state_projection_and_releases_its_permit() {
    let fixture =
        ProjectionWaitFixture::new(Duration::from_millis(100), Duration::from_secs(5)).await;
    let completion = tokio::time::timeout(TEST_TIMEOUT, fixture.ticket.complete()).await;
    let remaining = fixture.handle.status().in_flight;
    let registered = fixture.handle.pending_commands().len();
    let lease = tokio::time::timeout(Duration::from_millis(100), fixture.handle.exclusive()).await;
    let released = lease.is_ok();
    drop(lease);
    stop(&fixture.handle, fixture.task).await;

    let error = completion
        .expect("a native command deadline must also bound its projection barrier")
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert_eq!(remaining, 0);
    assert_eq!(registered, 0);
    assert!(
        released,
        "a timed-out barrier must release its runtime lease"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_projection_wait_releases_its_permit_without_waiting_for_the_deadline() {
    let fixture = ProjectionWaitFixture::new(Duration::from_secs(5), Duration::from_secs(5)).await;
    drop(fixture.ticket);
    let lease = tokio::time::timeout(Duration::from_millis(100), fixture.handle.exclusive()).await;
    let released = lease.is_ok();
    drop(lease);
    let remaining = fixture.handle.status().in_flight;
    let registered = fixture.handle.pending_commands().len();
    stop(&fixture.handle, fixture.task).await;

    assert!(
        released,
        "a cancelled projection wait must release its permit"
    );
    assert_eq!(remaining, 0);
    assert_eq!(registered, 0);
}

#[tokio::test(start_paused = true)]
async fn projection_wait_keeps_registration_until_the_watermark_completes() {
    let fixture = ProjectionWaitFixture::new(TEST_TIMEOUT, Duration::from_millis(100)).await;
    let registered = fixture
        .handle
        .pending_commands()
        .iter()
        .any(|command| command.token == fixture.ticket.token());
    tokio::time::timeout(TEST_TIMEOUT, fixture.ticket.complete())
        .await
        .unwrap()
        .unwrap();
    wait_for_no_in_flight(&fixture.handle).await;
    assert!(fixture.handle.pending_commands().is_empty());
    stop(&fixture.handle, fixture.task).await;

    assert!(
        registered,
        "native result alone must not retire a pending barrier"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_releases_a_command_already_transferred_to_its_projection_barrier() {
    let fixture = ProjectionWaitFixture::new(Duration::from_secs(5), Duration::from_secs(5)).await;
    stop(&fixture.handle, fixture.task).await;
    let error = tokio::time::timeout(TEST_TIMEOUT, fixture.ticket.complete())
        .await
        .unwrap()
        .unwrap_err();
    wait_for_no_in_flight(&fixture.handle).await;
    assert!(fixture.handle.pending_commands().is_empty());
    assert!(error.to_string().contains("event projector stopped"));
}

#[tokio::test(start_paused = true)]
async fn native_deadline_interrupts_a_blocked_guard_before_writer_admission() {
    let (transport, requests, _events) = test_transport(4);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::ZERO,
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(64, transport, config);
    let checked = Arc::new(CountingGuard::default());
    let guard = Arc::new(BlockingGuard {
        inner: checked.clone(),
        entered: Default::default(),
        release: Default::default(),
    });
    let mut guarded_command = command();
    guarded_command.metadata.dispatch_guard = Some(guard.clone());
    let ticket = handle.submit(guarded_command).await.unwrap();
    tokio::time::timeout(TEST_TIMEOUT, guard.entered.notified())
        .await
        .unwrap();
    let completion = tokio::time::timeout(Duration::from_millis(100), ticket.complete()).await;
    guard.release.notify_one();
    tokio::task::yield_now().await;
    let queued = requests.len();
    let checked = checked.0.load(std::sync::atomic::Ordering::SeqCst);
    let lease = tokio::time::timeout(Duration::from_millis(100), handle.exclusive()).await;
    let released = lease.is_ok();
    drop(lease);
    stop(&handle, task).await;

    let error = completion
        .expect("the native deadline must interrupt awaited guard preparation")
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert_eq!(queued, 0);
    assert_eq!(checked, 0);
    assert!(released);
}

#[tokio::test(start_paused = true)]
async fn native_deadline_interrupts_the_event_fence_before_checking_the_guard() {
    let (transport, requests, events) = test_transport(4);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::from_secs(5),
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(65, transport, config);
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"=breakpoint-modified,id=\"1\"\n",
        )))
        .await
        .unwrap();
    let guard = Arc::new(CountingGuard::default());
    let mut guarded_command = command();
    guarded_command.metadata.dispatch_guard = Some(guard.clone());
    let ticket = handle.submit(guarded_command).await.unwrap();
    wait_for_pending_running(&handle, ticket.token()).await;
    let completion = tokio::time::timeout(Duration::from_millis(100), ticket.complete()).await;
    let queued = requests.len();
    let checked = guard.0.load(std::sync::atomic::Ordering::SeqCst);
    stop(&handle, task).await;

    let error = completion
        .expect("the native deadline must interrupt a pending event fence")
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert_eq!(queued, 0);
    assert_eq!(checked, 0);
}

#[tokio::test(start_paused = true)]
async fn native_deadline_interrupts_unguarded_queue_insertion_without_a_late_write() {
    let (transport, requests, _events) = test_transport(1);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::ZERO,
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(66, transport, config);
    let first = handle.submit(command()).await.unwrap();
    wait_for_pending_running(&handle, first.token()).await;
    let second = handle.submit(command()).await.unwrap();
    wait_for_pending_running(&handle, second.token()).await;
    let completion = tokio::time::timeout(Duration::from_millis(100), async {
        tokio::join!(first.complete(), second.complete())
    })
    .await;

    // Making queue capacity available after the deadline must not resurrect
    // the second command's cancelled insertion future.
    let (_, written) = receive_write(&requests).await;
    written.send(Ok(())).unwrap();
    tokio::task::yield_now().await;
    let queued = requests.len();
    let remaining = handle.status().in_flight;
    let lease = tokio::time::timeout(Duration::from_millis(100), handle.exclusive()).await;
    let released = lease.is_ok();
    drop(lease);
    stop(&handle, task).await;

    let (first, second) =
        completion.expect("native deadlines must progress even while a writer queue is full");
    assert!(first.unwrap_err().to_string().contains("timed out"));
    assert!(second.unwrap_err().to_string().contains("timed out"));
    assert_eq!(queued, 0);
    assert_eq!(remaining, 0);
    assert!(released);
}

#[tokio::test]
async fn expired_ready_permit_cannot_admit_a_native_write() {
    #[derive(Debug)]
    struct DelayedAdmission;

    #[async_trait::async_trait]
    impl crate::cmd_flow::input::DispatchGuard for DelayedAdmission {
        async fn check(&self) -> Result<()> {
            Ok(())
        }

        async fn prepare(&self) -> Result<Box<dyn crate::cmd_flow::input::DispatchPermit>> {
            Ok(Box::new(DelayedAdmission))
        }
    }

    impl crate::cmd_flow::input::DispatchPermit for DelayedAdmission {
        fn dispatch(
            &mut self,
            validate: &mut dyn FnMut() -> Result<()>,
            action: &mut dyn FnMut() -> Result<()>,
        ) -> Result<()> {
            // A prepared permit can wait on a synchronous admission lock while
            // the command's async deadline timer cannot be polled.
            std::thread::sleep(Duration::from_millis(50));
            validate()?;
            action()
        }
    }

    let (transport, requests, _events) = test_transport(4);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::ZERO,
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(67, transport, config);
    let mut guarded_command = command();
    guarded_command.metadata.dispatch_guard = Some(Arc::new(DelayedAdmission));
    let ticket = handle.submit(guarded_command).await.unwrap();
    let error = tokio::time::timeout(TEST_TIMEOUT, ticket.complete())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(requests.is_empty());
    wait_for_no_in_flight(&handle).await;
    assert!(handle.pending_commands().is_empty());
    stop(&handle, task).await;
}

async fn assert_native_event_backpressure_deadline(during_guarded_drain: bool) {
    #[derive(Debug)]
    struct SaturatingProtocol {
        protocol: GdbMiProtocol,
        inject_on_encode: Option<flume::Sender<TransportEvent>>,
        decoded: Arc<tokio::sync::Notify>,
    }

    fn notifications() -> Bytes {
        Bytes::from(
            (0..EVENT_MAILBOX_CAPACITY + 8)
                .map(|id| format!("=breakpoint-modified,id=\"{id}\"\n"))
                .collect::<String>(),
        )
    }

    impl crate::debugger::protocol::DebuggerProtocol for SaturatingProtocol {
        fn encode_command(
            &self,
            command: crate::debugger::protocol::ProtocolCommand<'_>,
        ) -> Result<Bytes> {
            if let Some(events) = &self.inject_on_encode {
                events.try_send(TransportEvent::Stdout(notifications()))?;
            }
            self.protocol.encode_command(command)
        }

        fn push_stdout(
            &mut self,
            bytes: Bytes,
        ) -> Result<Vec<crate::debugger::protocol::ProtocolRecord>> {
            let records = self.protocol.push_stdout(bytes)?;
            self.decoded.notify_one();
            Ok(records)
        }
    }

    let (transport, requests, events) = test_transport(4);
    let config = RuntimeConfig {
        command_timeout: Duration::from_millis(25),
        sweep_interval: Duration::from_millis(5),
        projector_delay: Duration::from_secs(5),
        publisher_delay: Duration::ZERO,
    };
    let (lifecycle, mut terminations) = lifecycle::channel();
    let decoded = Arc::new(tokio::sync::Notify::new());
    let (handle, mut task) = SessionHandle::spawn_with_config(
        68,
        transport,
        Box::new(SaturatingProtocol {
            protocol: GdbMiProtocol::default(),
            inject_on_encode: during_guarded_drain.then(|| events.clone()),
            decoded: decoded.clone(),
        }),
        lifecycle.bind(68),
        test_reducer(),
        config,
    );
    let first = if during_guarded_drain {
        None
    } else {
        let ticket = handle.submit(command()).await.unwrap();
        let (_, written) = receive_write(&requests).await;
        written.send(Ok(())).unwrap();
        events
            .send_async(TransportEvent::Stdout(notifications()))
            .await
            .unwrap();
        tokio::time::timeout(TEST_TIMEOUT, decoded.notified())
            .await
            .unwrap();
        Some(ticket)
    };
    let guard = Arc::new(CountingGuard::default());
    let mut guarded_command = command();
    guarded_command.metadata.dispatch_guard = Some(guard.clone());
    let guarded = handle.submit(guarded_command).await.unwrap();
    let completion = tokio::time::timeout(Duration::from_millis(100), guarded.complete()).await;
    let stopped = tokio::time::timeout(Duration::from_millis(100), &mut task).await;
    let faulted = match stopped {
        Ok(result) => {
            result.unwrap();
            true
        }
        Err(_) => {
            stop(&handle, task).await;
            false
        }
    };
    if let Some(first) = first {
        assert!(first.complete().await.is_err());
    }
    let termination = tokio::time::timeout(Duration::from_millis(100), terminations.recv()).await;

    assert!(completion
        .expect("stalled native event processing must settle at the command deadline")
        .is_err());
    assert!(
        faulted,
        "partially decoded native events require session teardown"
    );
    assert!(matches!(
        termination.unwrap().unwrap().cause,
        SessionTerminationCause::ProtocolFault { .. }
    ));
    assert!(requests.is_empty());
    assert_eq!(guard.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(handle.status().in_flight, 0);
    assert!(handle.pending_commands().is_empty());
}

#[tokio::test(start_paused = true)]
async fn native_event_backpressure_faults_at_the_pending_deadline() {
    assert_native_event_backpressure_deadline(false).await;
}

#[tokio::test(start_paused = true)]
async fn guarded_event_drain_backpressure_faults_before_any_native_write() {
    assert_native_event_backpressure_deadline(true).await;
}

#[tokio::test]
async fn shutdown_interrupts_transport_backpressure() {
    let (transport, _requests, _events) = test_transport(1);
    let (handle, task, _terminations) = spawn_runtime(46, transport);

    let first = handle.submit(command()).await.unwrap();
    let second = handle.submit(command()).await.unwrap();
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
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(48, transport, config);
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
async fn continue_acknowledgment_does_not_overwrite_a_newer_native_stop() {
    use crate::{
        cmd_flow::{
            api::CommandExecutor,
            execution::ExecutionService,
            input::ParsedInputCmd,
            router::{Router, Target},
            transaction::TransactionCoordinator,
        },
        common::Config,
        debugger::gdb::backend::GdbBackend,
        feature::{proclet_query::ProcletQueryService, proclet_restore::ProcletRestorationMgr},
        plugin::resolve_framework_plugin,
        state::ThreadStatus,
    };
    let model = RuntimeModel::new();
    model.register_session(1, "fixture", None).await;
    model.register_thread_group(1, "i1").await.unwrap();
    model.register_thread(1, 1, "i1").await.unwrap();
    model
        .update_thread_statuses(1, &[1], ThreadStatus::STOPPED)
        .await
        .unwrap();
    let before = model.thread_snapshots_for_sessions(&[1]).await[0].execution_revision;
    let reducer = DebuggerEventReducer::new(
        model.clone(),
        BreakpointEventPublisher::new(
            Arc::new(NotificationManager::new()),
            crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
            crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
        ),
    );
    let (transport, requests, events) = test_transport(4);
    let (handle, task, _terminations) = spawn_runtime_with_reducer(1, transport, reducer);
    let router = Arc::new(Router::new(model.clone()));
    router.add_session(handle.clone());
    let executor = CommandExecutor::new(router.clone());
    let config = Arc::new(Config::from_str("Framework: unspecified\n").unwrap());
    let plugin = resolve_framework_plugin(&config);
    let queries = ProcletQueryService::connect(&config, plugin.as_ref())
        .await
        .unwrap();
    let service = ExecutionService::new(
        model.clone(),
        config,
        Arc::new(ProcletRestorationMgr::new(
            model.clone(),
            executor.clone(),
            queries,
        )),
        executor,
        TransactionCoordinator::new(model.clone(), router),
        Arc::new(GdbBackend),
    );
    let continuation = tokio::spawn(async move {
        service
            .continue_command(ParsedInputCmd {
                external_token: None,
                prefix: "-exec-continue".into(),
                args: String::new(),
                target: Target::Session(1),
                metadata: Default::default(),
            })
            .await
    });
    let (wire, written) = receive_write(&requests).await;
    written.send(Ok(())).unwrap();
    events.send_async(TransportEvent::Stdout(Bytes::from_static(
        b"*running,thread-id=\"all\"\n*stopped,reason=\"end-stepping-range\",thread-id=\"1\",stopped-threads=\"all\",frame={file=\"main.c\",line=\"7\"}\n"
    ))).await.unwrap();
    let stopped = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let state = model.thread_snapshots_for_sessions(&[1]).await.remove(0);
            if state.status == ThreadStatus::STOPPED && state.execution_revision > before {
                break state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let token = wire
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    events
        .send_async(TransportEvent::Stdout(Bytes::from(format!(
            "{token}^running\n"
        ))))
        .await
        .unwrap();
    tokio::time::timeout(TEST_TIMEOUT, continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        model.thread_snapshots_for_sessions(&[1]).await[0],
        stopped,
        "a command acknowledgment is not evidence of execution after a newer stop"
    );
    stop(&handle, task).await;
}

#[tokio::test]
async fn completion_mode_controls_state_projection_watermark() {
    let (transport, requests, events) = test_transport(4);
    let config = RuntimeConfig {
        command_timeout: TEST_TIMEOUT,
        sweep_interval: Duration::from_millis(10),
        projector_delay: Duration::from_millis(100),
        publisher_delay: Duration::ZERO,
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(47, transport, config);

    let mut state_consistent = command();
    state_consistent.consistency = CompletionConsistency::StateConsistent;
    let ticket = handle.submit(state_consistent).await.unwrap();
    let (_, acknowledgement) = receive_write(&requests).await;
    acknowledgement.send(Ok(())).unwrap();
    let completion = tokio::spawn(async move { ticket.complete().await });
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"=breakpoint-modified,id=\"1\"\n1^done\n",
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
    let protocol_complete = handle.submit(command()).await.unwrap();
    let (_, acknowledgement) = receive_write(&requests).await;
    acknowledgement.send(Ok(())).unwrap();
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"=breakpoint-modified,id=\"2\"\n2^done\n",
        )))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(50), protocol_complete.complete())
        .await
        .expect("protocol-complete response should not wait for projection")
        .unwrap();

    stop(&handle, task).await;
}

#[tokio::test]
async fn presentation_io_is_not_part_of_state_consistency() {
    let (transport, requests, events) = test_transport(2);
    let config = RuntimeConfig {
        command_timeout: TEST_TIMEOUT,
        sweep_interval: Duration::from_millis(10),
        projector_delay: Duration::ZERO,
        publisher_delay: Duration::from_millis(250),
    };
    let (handle, task, _terminations) = spawn_runtime_with_config(49, transport, config);

    let ticket = handle.submit(command()).await.unwrap();
    let (_, acknowledgement) = receive_write(&requests).await;
    acknowledgement.send(Ok(())).unwrap();
    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"*stopped,reason=\"end-stepping-range\"\n1^done\n",
        )))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_millis(100), ticket.complete())
        .await
        .expect("state-consistent response should wait for publication admission, not output")
        .unwrap();

    stop(&handle, task).await;
}

#[tokio::test]
async fn waits_for_protocol_ready_record_before_bootstrap_commands() {
    let (transport, _requests, events) = test_transport(2);
    let (lifecycle, _terminations) = lifecycle::channel();
    let (handle, task) = SessionHandle::spawn(
        51,
        transport,
        Box::new(LldbJsonProtocol::new("runtime-test")),
        lifecycle.bind(51),
        test_reducer(),
    );

    let waiting_handle = handle.clone();
    let readiness = tokio::spawn(async move { waiting_handle.wait_until_ready().await });
    tokio::task::yield_now().await;
    assert!(!readiness.is_finished());

    events
        .send_async(TransportEvent::Stdout(Bytes::from_static(
            b"@DDB@runtime-test@{\"type\":\"ready\",\"message\":\"ready\"}\n",
        )))
        .await
        .unwrap();

    tokio::time::timeout(TEST_TIMEOUT, readiness)
        .await
        .expect("runtime did not observe protocol readiness")
        .unwrap()
        .unwrap();
    stop(&handle, task).await;
}
