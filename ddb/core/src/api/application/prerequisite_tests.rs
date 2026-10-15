use super::*;

use bytes::Bytes;
use tokio::sync::oneshot;

use crate::{
    api::application::execution_guard::StopDispatchGuard,
    cmd_flow::{
        api::CommandExecutor,
        breakpoint::BreakpointEventPublisher,
        event::DebuggerEventReducer,
        execution::ExecutionService,
        input::{CommandMetadata, ParsedInputCmd},
        output_hub::{OutputDelivery, OutputSubscription},
        router::Router,
        session_runtime::SessionHandle,
        transaction::TransactionCoordinator,
    },
    common::Config,
    connection::{RunningTransport, TransportEvent, TransportRequest},
    debugger::gdb::{backend::GdbBackend, protocol::GdbMiProtocol},
    feature::{proclet_query::ProcletQueryService, proclet_restore::ProcletRestorationMgr},
    notification::NotificationManager,
    plugin::resolve_framework_plugin,
    session::lifecycle,
    state::{ThreadContext, ThreadStatus},
};

const TIMEOUT: Duration = Duration::from_secs(1);

struct Fixture {
    model: Arc<RuntimeModel>,
    service: Arc<ExecutionService>,
    restoration: Arc<ProcletRestorationMgr>,
    transactions: TransactionCoordinator,
    handle: SessionHandle,
    task: tokio::task::JoinHandle<()>,
    requests: flume::Receiver<TransportRequest>,
    events: flume::Sender<TransportEvent>,
    guard: Arc<dyn DispatchGuard>,
    active: Arc<Mutex<bool>>,
    output: Arc<OutputHub>,
    thread: GlobalThreadId,
    revision: u64,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_migration(false).await
    }

    async fn with_migration(migration: bool) -> Self {
        let model = RuntimeModel::new();
        model.register_session(1, "fixture", None).await;
        model.complete_session_activation(1, None).await;
        model.register_thread_group(1, "i1").await.unwrap();
        let thread = model.register_thread(1, 1, "i1").await.unwrap().thread_id;
        model
            .update_thread_statuses(1, &[1], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let revision = model.thread_snapshots_for_sessions(&[1]).await[0].stop_revision;
        model
            .enter_custom_context(
                1,
                ThreadContext {
                    tid: thread,
                    ctx: HashMap::from([("pc".into(), 42)]),
                },
            )
            .await;
        let output = OutputHub::new(Default::default());
        let reducer = DebuggerEventReducer::new(
            model.clone(),
            BreakpointEventPublisher::new(
                Arc::new(NotificationManager::new()),
                crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
                output.clone(),
            ),
        );
        let (requests, request_rx) = flume::bounded(8);
        let (event_tx, events) = flume::bounded(8);
        let (lifecycle, _terminations) = lifecycle::channel();
        let (handle, task) = SessionHandle::spawn(
            1,
            RunningTransport::new(requests, events),
            Box::new(GdbMiProtocol::default()),
            lifecycle.bind(1),
            reducer,
        );
        let router = Arc::new(Router::new(model.clone()));
        router.add_session(handle.clone());
        let executor = CommandExecutor::new(router.clone());
        let mut config = Config::from_str("Framework: unspecified\n").unwrap();
        config.conf.support_migration = migration;
        let config = Arc::new(config);
        let plugin = resolve_framework_plugin(&config);
        let queries = ProcletQueryService::connect(&config, plugin.as_ref())
            .await
            .unwrap();
        let restoration = Arc::new(ProcletRestorationMgr::new(
            model.clone(),
            executor.clone(),
            queries,
        ));
        let transactions = TransactionCoordinator::new(model.clone(), router);
        let service = Arc::new(ExecutionService::new(
            model.clone(),
            config,
            restoration.clone(),
            executor,
            transactions.clone(),
            Arc::new(GdbBackend),
        ));
        let active = Arc::new(Mutex::new(true));
        let epoch = model.execution_control_epoch();
        let guard = Arc::new(ActiveGuard {
            inner: Arc::new(LogpointGuard {
                stop: Arc::new(StopDispatchGuard {
                    model: model.clone(),
                    thread,
                    revision,
                }),
                expected: epoch.current(),
                epoch,
                output: output.clone(),
                session: 1,
                thread: thread.value(),
                message: "one logpoint\n".into(),
            }),
            active: active.clone(),
        });
        Self {
            model,
            service,
            restoration,
            transactions,
            handle,
            task,
            requests: request_rx,
            events: event_tx,
            guard,
            active,
            output,
            thread,
            revision,
        }
    }

    fn command(&self) -> ParsedInputCmd {
        ParsedInputCmd {
            external_token: None,
            prefix: "-record-time-and-continue".into(),
            args: String::new(),
            target: CommandTarget::Thread(self.thread),
            metadata: CommandMetadata {
                operation_id: Some("test-continuation".into()),
                operation_kind: Some(v2::OperationKind::Execute as u32),
                dispatch_guard: Some(self.guard.clone()),
            },
        }
    }

    async fn write(&self) -> (String, oneshot::Sender<anyhow::Result<()>>) {
        let request = tokio::time::timeout(TIMEOUT, self.requests.recv_async())
            .await
            .expect("native write was not dispatched")
            .unwrap();
        let TransportRequest::Write { data, written } = request;
        (String::from_utf8(data.to_vec()).unwrap(), written)
    }

    async fn response(&self, wire: &str, prefix: &str, payload: &str) {
        let command = wire.lines().find(|line| line.contains(prefix)).unwrap();
        let token = command
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        self.events
            .send_async(TransportEvent::Stdout(Bytes::from(format!(
                "{token}^{payload}\n"
            ))))
            .await
            .unwrap();
    }

    async fn stop(self) {
        tokio::time::timeout(TIMEOUT, self.handle.shutdown())
            .await
            .unwrap();
        tokio::time::timeout(TIMEOUT, self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn no_output(output: &mut OutputSubscription) {
    assert!(
        tokio::time::timeout(Duration::from_millis(20), output.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_or_stale_logpoint_does_not_restore_registers_after_queuing() {
    #[derive(Clone, Copy)]
    enum Invalidation {
        Cancelled,
        Expired,
        NewStop,
        Claimed,
    }
    for invalidation in [
        Invalidation::Cancelled,
        Invalidation::Expired,
        Invalidation::NewStop,
        Invalidation::Claimed,
    ] {
        let fixture = Fixture::new().await;
        let mut output = fixture.output.subscribe(None).unwrap();
        let lease = fixture.handle.exclusive().await.unwrap();
        let mut continuation = Box::pin(fixture.service.continue_command(fixture.command()));
        assert!(futures::poll!(continuation.as_mut()).is_pending());
        match invalidation {
            Invalidation::Cancelled => fixture.model.retire_stop_automation(),
            Invalidation::Expired => drop(AutomationLease(fixture.active.clone())),
            Invalidation::NewStop => fixture
                .model
                .update_thread_statuses(1, &[1], ThreadStatus::STOPPED)
                .await
                .unwrap(),
            Invalidation::Claimed => assert!(
                fixture
                    .model
                    .claim_thread_stop(fixture.thread, fixture.revision)
                    .await
            ),
        }
        drop(lease);
        let result = tokio::time::timeout(TIMEOUT, async {
            tokio::select! {
                result = continuation.as_mut() => result,
                _ = fixture.requests.recv_async() => panic!("superseded work restored native registers"),
            }
        })
        .await
        .unwrap();
        assert!(result.is_err());
        drop(continuation);
        assert!(fixture.requests.is_empty());
        assert!(
            fixture
                .model
                .session_snapshot(1)
                .await
                .unwrap()
                .in_custom_context
        );
        no_output(&mut output).await;
        let current = fixture.model.thread_snapshots_for_sessions(&[1]).await[0].stop_revision;
        assert_eq!(
            fixture
                .model
                .claim_thread_stop(fixture.thread, current)
                .await,
            !matches!(invalidation, Invalidation::Claimed)
        );
        fixture.stop().await;
    }
}

#[tokio::test]
async fn logpoint_context_restoration_preserves_its_resume_claim_and_outputs_once() {
    let fixture = Fixture::new().await;
    let mut output = fixture.output.subscribe(None).unwrap();
    let service = fixture.service.clone();
    let command = fixture.command();
    let continuation = tokio::spawn(async move { service.continue_command(command).await });
    let (restore, written) = fixture.write().await;
    assert!(restore.contains("-switch-context-custom pc=42"));
    no_output(&mut output).await;
    written.send(Ok(())).unwrap();
    fixture
        .response(
            &restore,
            "-switch-context-custom",
            "done,message=\"success\"",
        )
        .await;
    let (resume, written) = fixture.write().await;
    assert!(resume.contains("-record-time-and-continue"));
    written.send(Ok(())).unwrap();
    fixture
        .response(&resume, "-record-time-and-continue", "running")
        .await;
    tokio::time::timeout(TIMEOUT, continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let OutputDelivery::Record(record) = tokio::time::timeout(TIMEOUT, output.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("logpoint output was lost")
    };
    assert_eq!(record.text, "one logpoint\n");
    no_output(&mut output).await;
    assert!(
        !fixture
            .model
            .session_snapshot(1)
            .await
            .unwrap()
            .in_custom_context
    );
    assert!(
        !fixture
            .model
            .claim_thread_stop(fixture.thread, fixture.revision)
            .await
    );
    fixture.stop().await;
}

#[tokio::test]
async fn cancelled_migration_cleanup_preserves_heap_records_and_never_writes() {
    for invalidate in 0..4 {
        let fixture = Fixture::with_migration(true).await;
        fixture.restoration.seed_restored_heap_for_test(1, 7);
        let mut output = fixture.output.subscribe(None).unwrap();
        let lease = fixture.handle.exclusive().await.unwrap();
        let mut continuation = Box::pin(fixture.service.continue_command(fixture.command()));
        assert!(futures::poll!(continuation.as_mut()).is_pending());
        match invalidate {
            0 => fixture.model.retire_stop_automation(),
            1 => drop(AutomationLease(fixture.active.clone())),
            2 => fixture
                .model
                .update_thread_statuses(1, &[1], ThreadStatus::STOPPED)
                .await
                .unwrap(),
            _ => assert!(
                fixture
                    .model
                    .claim_thread_stop(fixture.thread, fixture.revision)
                    .await
            ),
        }
        drop(lease);
        let result = tokio::time::timeout(TIMEOUT, async {
            tokio::select! {
                result = continuation.as_mut() => result,
                _ = fixture.requests.recv_async() => panic!("superseded continuation cleaned a restored heap"),
            }
        }).await.unwrap();
        assert!(result.is_err());
        drop(continuation);
        assert_eq!(fixture.restoration.restored_heap_count_for_test(1), 1);
        assert!(fixture.requests.is_empty());
        no_output(&mut output).await;
        fixture.stop().await;
    }
}

#[tokio::test]
async fn successful_migration_cleanup_precedes_restoration_and_resume() {
    let fixture = Fixture::with_migration(true).await;
    fixture.restoration.seed_restored_heap_for_test(1, 7);
    let mut output = fixture.output.subscribe(None).unwrap();
    let service = fixture.service.clone();
    let command = fixture.command();
    let continuation = tokio::spawn(async move { service.continue_command(command).await });
    let (cleanup, written) = fixture.write().await;
    assert!(cleanup.contains("-clean-proclet-heap 7 4096"));
    no_output(&mut output).await;
    written.send(Ok(())).unwrap();
    fixture
        .response(&cleanup, "-clean-proclet-heap", "done,success=\"true\"")
        .await;
    let (restore, written) = fixture.write().await;
    assert!(restore.contains("-switch-context-custom pc=42"));
    assert_eq!(fixture.restoration.restored_heap_count_for_test(1), 0);
    no_output(&mut output).await;
    written.send(Ok(())).unwrap();
    fixture
        .response(
            &restore,
            "-switch-context-custom",
            "done,message=\"success\"",
        )
        .await;
    let (resume, written) = fixture.write().await;
    assert!(resume.contains("-record-time-and-continue"));
    written.send(Ok(())).unwrap();
    fixture
        .response(&resume, "-record-time-and-continue", "running")
        .await;
    tokio::time::timeout(TIMEOUT, continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let OutputDelivery::Record(record) = tokio::time::timeout(TIMEOUT, output.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("logpoint output was lost")
    };
    assert_eq!(record.text, "one logpoint\n");
    no_output(&mut output).await;
    fixture.stop().await;
}

#[tokio::test]
async fn failed_migration_cleanup_retains_heap_records_and_prevents_resume() {
    let fixture = Fixture::with_migration(true).await;
    fixture.restoration.seed_restored_heap_for_test(1, 7);
    fixture.restoration.seed_restored_heap_for_test(1, 8);
    let mut output = fixture.output.subscribe(None).unwrap();
    let service = fixture.service.clone();
    let command = fixture.command();
    let continuation = tokio::spawn(async move { service.continue_command(command).await });
    let (cleanup, written) = fixture.write().await;
    assert!(cleanup.contains("-clean-proclet-heap"));
    written.send(Ok(())).unwrap();
    fixture
        .response(&cleanup, "-clean-proclet-heap", "done,success=\"true\"")
        .await;
    let (failed_cleanup, written) = fixture.write().await;
    assert!(failed_cleanup.contains("-clean-proclet-heap"));
    assert_ne!(
        cleanup.split_once("-clean-proclet-heap").unwrap().1,
        failed_cleanup.split_once("-clean-proclet-heap").unwrap().1,
        "each restored heap has its own cleanup"
    );
    written.send(Ok(())).unwrap();
    fixture
        .response(
            &failed_cleanup,
            "-clean-proclet-heap",
            "done,success=\"false\",message=\"cleanup failed\"",
        )
        .await;
    assert!(tokio::time::timeout(TIMEOUT, continuation)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert_eq!(fixture.restoration.restored_heap_count_for_test(1), 1);
    assert_eq!(
        fixture.restoration.restoration_cache_counts_for_test(),
        (0, 0)
    );
    assert!(fixture.requests.is_empty());
    assert!(
        fixture
            .model
            .session_snapshot(1)
            .await
            .unwrap()
            .in_custom_context
    );
    assert!(
        fixture
            .model
            .claim_thread_stop(fixture.thread, fixture.revision)
            .await
    );
    no_output(&mut output).await;
    fixture.stop().await;
}

#[tokio::test]
async fn migration_cleanup_holds_the_session_lease_through_receipt_removal() {
    let fixture = Fixture::with_migration(true).await;
    fixture.restoration.seed_restored_heap_for_test(1, 7);
    let blocker = fixture.handle.exclusive().await.unwrap();
    let metadata = fixture.command().metadata;
    let mut cleanup = Box::pin(
        fixture
            .restoration
            .reset_guarded(&metadata, &fixture.transactions),
    );
    assert!(futures::poll!(cleanup.as_mut()).is_pending());
    // A backtrace can restore another heap before the queued cleanup obtains
    // the lease. Both its receipt and its locality cache must be resampled.
    fixture.restoration.seed_restored_heap_for_test(1, 8);
    drop(blocker);
    assert!(futures::poll!(cleanup.as_mut()).is_pending());
    assert_eq!(fixture.restoration.restoration_cache_counts_for_test().0, 0);
    let mut competitor = Box::pin(fixture.handle.exclusive());
    for _ in 0..2 {
        let (wire, written) = fixture.write().await;
        assert!(wire.contains("-clean-proclet-heap"));
        written.send(Ok(())).unwrap();
        fixture
            .response(&wire, "-clean-proclet-heap", "done,success=\"true\"")
            .await;
        tokio::time::timeout(TIMEOUT, async {
            while fixture.handle.status().in_flight != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            futures::poll!(competitor.as_mut()).is_pending(),
            "native acknowledgment must not release the bookkeeping lease"
        );
        if fixture.restoration.restored_heap_count_for_test(1) == 2 {
            assert!(futures::poll!(cleanup.as_mut()).is_pending());
        }
    }
    tokio::time::timeout(TIMEOUT, cleanup)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.restoration.restored_heap_count_for_test(1), 0);
    let competitor = tokio::time::timeout(TIMEOUT, competitor)
        .await
        .unwrap()
        .unwrap();
    fixture.restoration.seed_restored_heap_for_test(1, 7);
    assert_eq!(
        fixture.restoration.restored_heap_count_for_test(1),
        1,
        "a later identical restoration must retain its own receipt"
    );
    drop(competitor);
    fixture.stop().await;
}

#[tokio::test]
async fn retired_peer_heap_receipts_do_not_block_a_live_guarded_resume() {
    let fixture = Fixture::with_migration(true).await;
    fixture
        .model
        .register_session(2, "retired-peer", None)
        .await;
    fixture.model.complete_session_activation(2, None).await;
    fixture.restoration.seed_restored_heap_for_test(2, 7);
    // An unavailable native handle is still an unknown cleanup failure while
    // the model considers its session live; retain the receipt for retry.
    assert!(fixture
        .service
        .continue_command(fixture.command())
        .await
        .is_err());
    assert_eq!(fixture.restoration.restored_heap_count_for_test(2), 1);
    assert!(fixture.requests.is_empty());
    drop(
        fixture
            .model
            .begin_session_retirement(2)
            .await
            .finish()
            .await,
    );
    assert!(fixture.model.session_snapshot(2).await.is_none());
    let mut output = fixture.output.subscribe(None).unwrap();
    let service = fixture.service.clone();
    let command = fixture.command();
    let continuation = tokio::spawn(async move { service.continue_command(command).await });

    let (restore, written) = fixture.write().await;
    assert!(restore.contains("-switch-context-custom pc=42"));
    assert!(!restore.contains("-clean-proclet-heap"));
    assert_eq!(fixture.restoration.restored_heap_count_for_test(2), 0);
    assert_eq!(
        fixture.restoration.restoration_cache_counts_for_test(),
        (0, 0)
    );
    no_output(&mut output).await;
    written.send(Ok(())).unwrap();
    fixture
        .response(
            &restore,
            "-switch-context-custom",
            "done,message=\"success\"",
        )
        .await;
    let (resume, written) = fixture.write().await;
    assert!(resume.contains("-record-time-and-continue"));
    written.send(Ok(())).unwrap();
    fixture
        .response(&resume, "-record-time-and-continue", "running")
        .await;
    tokio::time::timeout(TIMEOUT, continuation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let OutputDelivery::Record(record) = tokio::time::timeout(TIMEOUT, output.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("logpoint output was lost")
    };
    assert_eq!(record.text, "one logpoint\n");
    no_output(&mut output).await;
    fixture.stop().await;
}
