mod support;

use std::time::Duration;

use ddb_api_client::{
    v2, ClientConfig, ClientError, DdbClient, OutputSyncItem, OutputSyncOptions,
    ProjectedStateSyncItem, StateSyncOptions,
};
use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN};

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v2-sdk",
        alias: "sdk-frontend",
        hash: "api-v2-sdk-group",
        pid: 1702,
        start_delay_ms: 0,
        source_file: "tests/api_v2_client.rs",
        source_line: 20,
        function: "mock_session",
        exit_on_continue: false,
    }
}

fn thread_target(thread_id: String) -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Thread(v2::ThreadTarget { thread_id })),
    }
}

#[test]
fn public_rust_sdk_negotiates_mutates_reconnects_and_converges() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let endpoint = ddb.api_endpoint();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("SDK test runtime should build");
    runtime.block_on(async move {
        let unauthenticated = DdbClient::new(ClientConfig::new(&endpoint)).unwrap();
        let error = unauthenticated
            .get_capabilities(v2::GetCapabilitiesRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Api { ref error, .. }
                if error.code == v2::DdbErrorCode::Unauthenticated as i32
        ));

        let mut config = ClientConfig::new(endpoint).with_bearer_token(V2_TEST_CONTROL_TOKEN);
        config.request_timeout = Duration::from_millis(100);
        let client = DdbClient::new(config).unwrap();
        let (server, capabilities) = client.handshake().await.unwrap();
        assert_eq!(capabilities.api_version, "v2");
        assert_eq!(capabilities.schema_version, "2.0.0-draft.3");
        assert_eq!(server.server_instance_id, capabilities.server_instance_id);
        assert!(capabilities.capabilities_id.starts_with("cap_"));

        let sessions = client.list_all_sessions(16).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].session_id.starts_with("ses_"));

        let mut sync = client
            .projected_state_sync(StateSyncOptions {
                sections: vec![
                    v2::SnapshotSection::Topology as i32,
                    v2::SnapshotSection::Selection as i32,
                    v2::SnapshotSection::Execution as i32,
                    v2::SnapshotSection::Breakpoints as i32,
                    v2::SnapshotSection::PendingOperations as i32,
                    v2::SnapshotSection::Capabilities as i32,
                ],
                ..Default::default()
            })
            .unwrap();
        let ProjectedStateSyncItem::Snapshot = sync.next().await.unwrap() else {
            panic!("first successful sync item should be a snapshot");
        };
        let projection = sync
            .projection()
            .expect("projected state sync should own the hydrated projection");
        assert_eq!(projection.sessions().len(), 1);
        assert!(projection.capabilities().is_some());

        let threads = client
            .list_threads(v2::ListThreadsRequest {
                context: None,
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                        session_id: sessions[0].session_id.clone(),
                    })),
                }),
                page: None,
            })
            .await
            .unwrap()
            .threads;
        assert_eq!(threads.len(), 1);
        let thread_id = threads[0].thread_id.clone();

        // A finite unary timeout must not become a lifetime limit for streams.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let request = v2::ExecuteRequest {
            context: Some(v2::RequestContext {
                idempotency_key: Some("sdk-next-idempotency".to_string()),
                ..Default::default()
            }),
            target: Some(thread_target(thread_id.clone())),
            action: v2::ExecutionAction::Next as i32,
            ..Default::default()
        };
        let first = client.execute(request.clone()).await.unwrap();
        let duplicate = client.execute(request).await.unwrap();
        let operation_id = first.operation.unwrap().operation_id;
        assert_eq!(
            duplicate.operation.unwrap().operation_id,
            operation_id,
            "the SDK must preserve caller idempotency keys across attempts"
        );

        // Simulate a broken frontend connection before consuming the change.
        // Replay from the snapshot cursor must recover both the operation and
        // stopped execution location without issuing another mutation.
        sync.force_reconnect();
        let mut observed = Vec::new();
        let convergence = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match sync.next().await.unwrap() {
                    ProjectedStateSyncItem::Snapshot => {
                        observed.push("snapshot".to_string());
                    }
                    ProjectedStateSyncItem::Event(event) => {
                        observed.push(format!(
                            "event kind={} resource={} id={} revision={}",
                            event.kind,
                            event.resource_kind,
                            event.resource_id,
                            event.resource_revision
                        ));
                    }
                    ProjectedStateSyncItem::Reconnecting { .. } => continue,
                    ProjectedStateSyncItem::Rehydrating { .. } => {
                        continue;
                    }
                }

                let projection = sync
                    .projection()
                    .expect("projection should be available after a delivered state item");
                let operation_complete =
                    projection
                        .operations()
                        .get(&operation_id)
                        .is_some_and(|operation| {
                            v2::OperationState::try_from(operation.state)
                                == Ok(v2::OperationState::Completed)
                        });
                let stepped = projection.execution_states().values().any(|state| {
                    !state.running
                        && state
                            .location
                            .as_ref()
                            .is_some_and(|location| location.line == 21)
                });
                if operation_complete && stepped {
                    break;
                }
            }
        })
        .await;
        assert!(
            convergence.is_ok(),
            "SDK replay did not converge; observed={observed:#?}; operations={:#?}; execution={:#?}",
            sync.projection().map(|projection| projection.operations()),
            sync.projection()
                .map(|projection| projection.execution_states())
        );

        let operation = client
            .wait_operation(
                operation_id,
                Duration::from_secs(2),
                Duration::from_millis(10),
            )
            .await
            .unwrap();
        assert_eq!(
            v2::OperationState::try_from(operation.state),
            Ok(v2::OperationState::Completed)
        );

        let mut output = client
            .output_sync(OutputSyncOptions {
                reconnect_initial_delay: Duration::from_millis(10),
                reconnect_max_delay: Duration::from_millis(100),
                max_reconnect_attempts: Some(4),
                ..Default::default()
            })
            .unwrap();
        let trigger_output = |key: &'static str| {
            let client = client.clone();
            let target = thread_target(thread_id.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let admitted = client
                    .execute_raw_command(v2::ExecuteRawCommandRequest {
                        context: Some(v2::RequestContext {
                            idempotency_key: Some(key.to_string()),
                            ..Default::default()
                        }),
                        target: Some(target),
                        dialect: v2::RawCommandDialect::GdbMi as i32,
                        command: "-mock-stream-output".to_string(),
                        preconditions: None,
                    })
                    .await
                    .unwrap();
                client
                    .wait_operation(
                        admitted.operation.unwrap().operation_id,
                        Duration::from_secs(2),
                        Duration::from_millis(10),
                    )
                    .await
                    .unwrap();
            })
        };

        let first_trigger = trigger_output("sdk-output-first");
        let first_output = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match output.next().await.unwrap() {
                    OutputSyncItem::Event(event) => break event,
                    OutputSyncItem::Reconnecting { .. } | OutputSyncItem::Restarting { .. } => {}
                }
            }
        })
        .await
        .expect("SDK output subscription should deliver debugger output");
        first_trigger.await.unwrap();
        assert_eq!(
            first_output.content,
            Some(v2::output_event::Content::Text(
                "mock console output\n".to_string()
            ))
        );
        assert_eq!(
            v2::OutputStreamKind::try_from(first_output.stream),
            Ok(v2::OutputStreamKind::Console)
        );

        // Asking for another item acknowledges the prior cursor. A forced
        // transport break must resume after it, not duplicate the first event.
        output.force_reconnect();
        let second_trigger = trigger_output("sdk-output-second");
        let second_output = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match output.next().await.unwrap() {
                    OutputSyncItem::Event(event) => break event,
                    OutputSyncItem::Reconnecting { .. } | OutputSyncItem::Restarting { .. } => {}
                }
            }
        })
        .await
        .expect("SDK output subscription should resume after a forced disconnect");
        second_trigger.await.unwrap();
        assert_eq!(
            second_output.content,
            Some(v2::output_event::Content::Text(
                "mock console output\n".to_string()
            ))
        );
        assert!(
            second_output.cursor.as_ref().unwrap().sequence
                > first_output.cursor.as_ref().unwrap().sequence,
            "resumed output sync must not redeliver an acknowledged event"
        );
    });
}
