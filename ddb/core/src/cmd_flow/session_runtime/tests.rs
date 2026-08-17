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
