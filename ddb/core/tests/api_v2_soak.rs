mod support;

use std::{env, time::Duration};

use ddb_api_client::{
    v2, ClientConfig, ClientError, DdbClient, OutputSyncItem, OutputSyncOptions,
    ProjectedStateSyncItem, StateSyncOptions,
};
use support::{DdbProcess, SessionSpec, V2_TEST_ADMIN_TOKEN, V2_TEST_CONTROL_TOKEN};

const STEP_ITERATIONS_DEFAULT: usize = 96;
const OUTPUT_ITERATIONS_DEFAULT: usize = 2_048;
const WAIT: Duration = Duration::from_secs(10);

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v2-soak",
        alias: "soak-frontend",
        hash: "api-v2-soak-group",
        pid: 2_702,
        start_delay_ms: 0,
        source_file: "tests/api_v2_soak.rs",
        source_line: 20,
        function: "mock_session",
        exit_on_continue: false,
    }
}

fn iterations(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn thread_target(thread_id: impl Into<String>) -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Thread(v2::ThreadTarget {
            thread_id: thread_id.into(),
        })),
    }
}

async fn wait_for_operation(client: &DdbClient, admission: v2::OperationAdmissionResponse) {
    let operation_id = admission
        .operation
        .expect("mutation admission should contain an operation")
        .operation_id;
    let operation = client
        .wait_operation(operation_id, WAIT, Duration::from_millis(5))
        .await
        .expect("Mock operation should become terminal");
    assert_eq!(
        v2::OperationState::try_from(operation.state),
        Ok(v2::OperationState::Completed)
    );
}

#[test]
#[ignore = "release soak: exercises bounded rollover and slow-client behavior"]
fn public_sdk_recovers_after_rollover_and_slow_output_without_blocking_control() {
    let mut ddb = DdbProcess::spawn_with_v2_conf(
        &[mock_session()],
        "  ApiLimits:\n    state_replay_events: 32\n    state_replay_bytes: 1048576\n    state_subscriber_queue: 4\n    output_subscriber_queue: 4\n    max_subscribers: 4\n    operation_records: 16\n    operation_bytes: 1048576\n    operation_record_bytes: 65536\n    output_event_bytes: 4096",
    );
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let endpoint = ddb.api_endpoint();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("soak runtime should build");
    runtime.block_on(async move {
        let admin_endpoint = endpoint.clone();
        let mut config = ClientConfig::new(endpoint).with_bearer_token(V2_TEST_CONTROL_TOKEN);
        config.request_timeout = Duration::from_secs(2);
        let client = DdbClient::new(config).expect("public SDK client should build");
        let sessions = client
            .list_all_sessions(16)
            .await
            .expect("session discovery should succeed");
        let threads = client
            .list_threads(v2::ListThreadsRequest {
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                        session_id: sessions[0].session_id.clone(),
                    })),
                }),
                ..Default::default()
            })
            .await
            .expect("thread discovery should succeed")
            .threads;
        let thread_id = threads[0].thread_id.clone();

        let sections = vec![
            v2::SnapshotSection::Topology as i32,
            v2::SnapshotSection::Execution as i32,
            v2::SnapshotSection::PendingOperations as i32,
        ];
        let mut state = client
            .projected_state_sync(StateSyncOptions {
                sections: sections.clone(),
                reconnect_initial_delay: Duration::from_millis(5),
                reconnect_max_delay: Duration::from_millis(20),
                max_reconnect_attempts: Some(8),
                ..Default::default()
            })
            .expect("state sync should build");
        assert!(matches!(
            state
                .next()
                .await
                .expect("initial hydration should succeed"),
            ProjectedStateSyncItem::Snapshot
        ));

        let step_iterations = iterations("DDB_API_SOAK_STEP_ITERATIONS", STEP_ITERATIONS_DEFAULT);
        let mut first_operation_id = None;
        for index in 0..step_iterations {
            let admission = client
                .execute(v2::ExecuteRequest {
                    context: Some(v2::RequestContext {
                        idempotency_key: Some(format!("soak-step-{index}")),
                        ..Default::default()
                    }),
                    target: Some(thread_target(thread_id.clone())),
                    action: v2::ExecutionAction::Next as i32,
                    ..Default::default()
                })
                .await
                .expect("step should be admitted");
            first_operation_id.get_or_insert_with(|| {
                admission
                    .operation
                    .as_ref()
                    .expect("admission should contain an operation")
                    .operation_id
                    .clone()
            });
            wait_for_operation(&client, admission).await;
        }

        let first_operation_id = first_operation_id.expect("at least one step should run");
        let expired = client
            .get_operation(v2::GetOperationRequest {
                operation_id: first_operation_id,
                ..Default::default()
            })
            .await
            .expect_err("old terminal operation should be evicted under configured churn");
        assert!(matches!(
            expired,
            ClientError::Api { ref error, .. }
                if error.code == v2::DdbErrorCode::NotFound as i32
        ));

        state.force_reconnect();
        let mut rehydrating = false;
        tokio::time::timeout(WAIT, async {
            loop {
                match state
                    .next()
                    .await
                    .expect("rollover recovery should continue")
                {
                    ProjectedStateSyncItem::Rehydrating { .. } => rehydrating = true,
                    ProjectedStateSyncItem::Snapshot if rehydrating => break,
                    ProjectedStateSyncItem::Snapshot
                    | ProjectedStateSyncItem::Event(_)
                    | ProjectedStateSyncItem::Reconnecting { .. } => {}
                }
            }
        })
        .await
        .expect("old cursor should trigger bounded rehydration promptly");
        assert!(rehydrating, "journal rollover must be explicit to the SDK");

        // Operation completion is backend acknowledgement; the corresponding
        // stopped event may commit later. Sample until that state is stable,
        // then prove replay/live delivery advances the SDK projection to the
        // same resource revisions.
        let fresh = tokio::time::timeout(WAIT, async {
            let mut previous = None;
            let mut stable_samples = 0;
            loop {
                let snapshot = client
                    .get_snapshot(v2::GetSnapshotRequest {
                        sections: sections.clone(),
                        ..Default::default()
                    })
                    .await
                    .expect("fresh comparison snapshot should succeed")
                    .snapshot
                    .expect("snapshot response should contain a snapshot");
                if previous.as_ref() == Some(&snapshot.execution_states) {
                    stable_samples += 1;
                } else {
                    stable_samples = 0;
                    previous = Some(snapshot.execution_states.clone());
                }
                if stable_samples >= 2 {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Mock stopped state should settle after operation acknowledgement");

        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(projected) = state.projection().map(|state| state.snapshot()) {
                    if projected.execution_states == fresh.execution_states {
                        assert_eq!(projected.sessions, fresh.sessions);
                        assert_eq!(projected.threads, fresh.threads);
                        break;
                    }
                }
                state
                    .next()
                    .await
                    .expect("projection should consume retained stop events");
            }
        })
        .await
        .expect("rehydrated projection should converge to stable backend state");

        let mut output = client
            .output_sync(OutputSyncOptions {
                reconnect_initial_delay: Duration::from_millis(5),
                reconnect_max_delay: Duration::from_millis(20),
                max_reconnect_attempts: Some(8),
                ..Default::default()
            })
            .expect("output sync should build");
        let first_client = client.clone();
        let first_target = thread_target(thread_id.clone());
        let first_output = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let admission = first_client
                .execute_raw_command(v2::ExecuteRawCommandRequest {
                    context: Some(v2::RequestContext {
                        idempotency_key: Some("soak-output-prime".to_string()),
                        ..Default::default()
                    }),
                    target: Some(first_target),
                    dialect: v2::RawCommandDialect::GdbMi as i32,
                    command: "-mock-stream-output".to_string(),
                    ..Default::default()
                })
                .await
                .expect("priming output command should be admitted");
            wait_for_operation(&first_client, admission).await;
        });
        let primed = tokio::time::timeout(WAIT, async {
            loop {
                if let OutputSyncItem::Event(event) =
                    output.next().await.expect("output should flow")
                {
                    break event;
                }
            }
        })
        .await
        .expect("output stream should be primed");
        assert!(primed.gap.is_none());
        first_output.await.expect("priming task should complete");

        let output_iterations =
            iterations("DDB_API_SOAK_OUTPUT_ITERATIONS", OUTPUT_ITERATIONS_DEFAULT);
        let admission = client
            .execute_raw_command(v2::ExecuteRawCommandRequest {
                context: Some(v2::RequestContext {
                    idempotency_key: Some("soak-output-bulk".to_string()),
                    ..Default::default()
                }),
                target: Some(thread_target(thread_id.clone())),
                dialect: v2::RawCommandDialect::GdbMi as i32,
                command: format!("-mock-stream-output {output_iterations} 4096"),
                ..Default::default()
            })
            .await
            .expect("bulk output churn command should be admitted");
        wait_for_operation(&client, admission).await;
        let gap = tokio::time::timeout(WAIT, async {
            loop {
                if let OutputSyncItem::Event(event) = output
                    .next()
                    .await
                    .expect("slow output consumer should receive a loss record")
                {
                    if event.gap.is_some() {
                        break event;
                    }
                }
            }
        })
        .await
        .expect("slow output loss must be reported without blocking control");
        assert!(gap
            .gap
            .expect("gap was checked")
            .dropped_events
            .is_some_and(|events| events > 0));

        let admin = DdbClient::new(
            ClientConfig::new(admin_endpoint).with_bearer_token(V2_TEST_ADMIN_TOKEN),
        )
        .expect("admin SDK client should build");
        admin
            .shutdown(v2::ShutdownRequest {
                context: Some(v2::RequestContext {
                    idempotency_key: Some("soak-graceful-shutdown".to_string()),
                    ..Default::default()
                }),
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Broadcast(v2::BroadcastTarget {})),
                }),
                ..Default::default()
            })
            .await
            .expect("public ADMIN shutdown should be admitted");
    });

    let status = ddb.wait_for_exit();
    assert!(status.success(), "DDB should drain after soak: {status}");
}
