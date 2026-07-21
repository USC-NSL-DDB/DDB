//! Behavioral tests for the session runtime actor.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use super::*;
use crate::cmd_flow::event::DebuggerEventReducer;

use crate::session::lifecycle::{self, SessionTermination, SessionTerminationCause};
use crate::{
    cmd_flow::breakpoint::BreakpointEventPublisher,
    connection::{RunningTransport, TransportEvent, TransportRequest},
    notification::NotificationManager,
    state::RuntimeModel,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(1);

fn test_reducer() -> Arc<DebuggerEventReducer> {
    DebuggerEventReducer::new(
        RuntimeModel::new(),
        BreakpointEventPublisher::new(
            Arc::new(NotificationManager::new()),
            crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
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
    let (handle, task) = SessionHandle::spawn(sid, transport, lifecycle.bind(sid), test_reducer());
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
