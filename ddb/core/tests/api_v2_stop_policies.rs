mod support;

use ddb_api_client::{v2, ClientConfig, ClientError, DdbClient, NdjsonStream};
use std::time::Duration;
use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN};

fn session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "stop-policies",
        alias: "worker",
        hash: "stop-policy-group",
        pid: 1740,
        start_delay_ms: 0,
        source_file: "main.rs",
        source_line: 20,
        function: "main",
        exit_on_continue: false,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn client(endpoint: &str) -> DdbClient {
    DdbClient::new(ClientConfig::new(endpoint).with_bearer_token(V2_TEST_CONTROL_TOKEN)).unwrap()
}

fn thread_target(id: &str) -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Thread(v2::ThreadTarget {
            thread_id: id.into(),
        })),
    }
}

fn broadcast() -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Broadcast(v2::BroadcastTarget {})),
    }
}

async fn complete(client: &DdbClient, admission: v2::OperationAdmissionResponse) -> v2::Operation {
    client
        .wait_operation(
            admission.operation.unwrap().operation_id,
            Duration::from_secs(5),
            Duration::from_millis(10),
        )
        .await
        .unwrap()
}

async fn snapshot(client: &DdbClient) -> v2::Snapshot {
    client
        .get_snapshot(v2::GetSnapshotRequest {
            sections: vec![
                v2::SnapshotSection::Topology as i32,
                v2::SnapshotSection::Execution as i32,
                v2::SnapshotSection::Breakpoints as i32,
                v2::SnapshotSection::StopPolicy as i32,
            ],
            ..Default::default()
        })
        .await
        .unwrap()
        .snapshot
        .unwrap()
}

async fn log_output(
    client: &DdbClient,
    stream: &mut NdjsonStream<v2::OutputEvent>,
    prefix: &str,
) -> v2::OutputEvent {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = stream.next().await.unwrap().unwrap();
            if matches!(&event.content, Some(v2::output_event::Content::Text(text)) if text.starts_with(prefix)) { return event }
        }
    }).await;
    match result {
        Ok(event) => event,
        Err(_) => panic!(
            "backend logpoint produced no output; snapshot={:#?}; operations={:#?}",
            snapshot(client).await,
            client
                .list_operations(v2::ListOperationsRequest::default())
                .await
        ),
    }
}

async fn two_client_logpoint(endpoint: &str, source: &str, line: u32, temporary: bool) {
    let first = client(endpoint);
    let second = client(endpoint);
    let initial = snapshot(&first).await;
    let session = initial.sessions[0].session_id.clone();
    let target = v2::Target {
        selector: Some(v2::target::Selector::Session(v2::SessionTarget {
            session_id: session.clone(),
        })),
    };
    let filter = v2::SubscribeOutputRequest {
        filter: Some(v2::OutputFilter {
            streams: vec![v2::OutputStreamKind::Console as i32],
            session_ids: vec![session.clone()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut first_output = first.subscribe_output(filter.clone()).await.unwrap();
    let mut thread_filter = filter;
    thread_filter.filter.as_mut().unwrap().thread_ids = vec![initial.threads[0].thread_id.clone()];
    let mut second_output = second.subscribe_output(thread_filter).await.unwrap();
    let created = complete(
        &first,
        first
            .create_breakpoint(v2::CreateBreakpointRequest {
                target: Some(target.clone()),
                breakpoint: Some(v2::BreakpointSpec {
                    location: Some(v2::breakpoint_spec::Location::Source(
                        v2::SourceBreakpointLocation {
                            source: source.into(),
                            line,
                            column: 0,
                        },
                    )),
                    log_message: Some("shared counter={counter}".into()),
                    temporary,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        created.state,
        v2::OperationState::Completed as i32,
        "{created:?}"
    );
    let Some(v2::operation_result::Value::Breakpoint(breakpoint)) = created.result.unwrap().value
    else {
        panic!("missing breakpoint")
    };
    let shared = snapshot(&second).await;
    assert_eq!(
        shared.breakpoints[0]
            .spec
            .as_ref()
            .unwrap()
            .log_message
            .as_deref(),
        Some("shared counter={counter}")
    );
    let resumed = complete(
        &first,
        first
            .execute(v2::ExecuteRequest {
                target: Some(target.clone()),
                action: v2::ExecutionAction::Continue as i32,
                ..Default::default()
            })
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        resumed.state,
        v2::OperationState::Completed as i32,
        "{resumed:?}"
    );
    let (a, b) = tokio::join!(
        log_output(&first, &mut first_output, "shared counter="),
        log_output(&second, &mut second_output, "shared counter=")
    );
    assert_eq!(
        a, b,
        "two clients must observe the same backend output record"
    );
    assert_eq!(a.session_id.as_deref(), Some(session.as_str()));
    assert!(a
        .thread_id
        .as_ref()
        .is_some_and(|id| initial.threads.iter().any(|thread| &thread.thread_id == id)));
    assert!(
        matches!(&a.content, Some(v2::output_event::Content::Text(text)) if text.ends_with('\n'))
    );
    if !temporary {
        let cleared = complete(
            &second,
            second
                .update_breakpoint(v2::UpdateBreakpointRequest {
                    breakpoint_id: breakpoint.breakpoint_id,
                    target: Some(target.clone()),
                    breakpoint: Some(v2::BreakpointSpec::default()),
                    update_mask: Some(ddb_api_client::wkt::FieldMask {
                        paths: vec!["log_message".into()],
                    }),
                    ..Default::default()
                })
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            cleared.state,
            v2::OperationState::Completed as i32,
            "{cleared:?}"
        );
        assert!(snapshot(&first).await.breakpoints[0]
            .spec
            .as_ref()
            .unwrap()
            .log_message
            .is_none());
    }
    let paused = complete(
        &second,
        second
            .execute(v2::ExecuteRequest {
                target: Some(target),
                action: v2::ExecutionAction::Interrupt as i32,
                ..Default::default()
            })
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        paused.state,
        v2::OperationState::Completed as i32,
        "{paused:?}"
    );
}

#[test]
fn two_headless_clients_share_logpoints_and_reject_duplicate_stop_controls() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    runtime().block_on(async {
        let first = client(&ddb.api_endpoint());
        let second = client(&ddb.api_endpoint());
        let before = snapshot(&first).await;
        let target = thread_target(&before.threads[0].thread_id);
        let state = first
            .get_execution_state(v2::GetExecutionStateRequest {
                target: Some(target.clone()),
                ..Default::default()
            })
            .await
            .unwrap()
            .execution_state
            .unwrap();
        let request = v2::ExecuteRequest {
            target: Some(target),
            action: v2::ExecutionAction::Next as i32,
            execution_guard: Some(v2::ExecutionGuard {
                server_instance_id: before.server_instance_id,
                execution_state_id: state.execution_state_id,
                revision: state.revision,
            }),
            ..Default::default()
        };
        let (a, b) = tokio::join!(first.execute(request.clone()), second.execute(request));
        let mut succeeded = 0;
        for response in [a, b] {
            match response {
                Ok(admission) => {
                    let operation = complete(&first, admission).await;
                    if operation.state == v2::OperationState::Completed as i32 {
                        succeeded += 1
                    } else {
                        assert_eq!(
                            operation.error.unwrap().code,
                            v2::DdbErrorCode::FailedPrecondition as i32
                        );
                    }
                }
                Err(ClientError::Api { error, .. }) => {
                    assert_eq!(error.code, v2::DdbErrorCode::FailedPrecondition as i32)
                }
                other => panic!("unexpected response: {other:?}"),
            }
        }
        assert_eq!(succeeded, 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if snapshot(&first)
                    .await
                    .threads
                    .iter()
                    .all(|thread| thread.state == v2::ThreadState::Stopped as i32)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        two_client_logpoint(&ddb.api_endpoint(), "main.rs", 20, false).await;
    });
}

#[test]
fn queued_broadcasts_from_a_previous_backend_are_rejected_after_restart() {
    let old_instance = {
        let mut ddb = DdbProcess::spawn_with_v2_auth(&[session()]);
        ddb.wait_for_sessions_len(1);
        runtime().block_on(async {
            snapshot(&client(&ddb.api_endpoint()))
                .await
                .server_instance_id
        })
    };
    let mut replacement = DdbProcess::spawn_with_v2_auth(&[session()]);
    replacement.wait_for_sessions_len(1);
    replacement.wait_for_stdout_count("*stopped", 1);
    runtime().block_on(async {
        let current = client(&replacement.api_endpoint());
        let queued = current.for_server_instance(old_instance.clone());
        let error = queued.execute(v2::ExecuteRequest { target: Some(broadcast()),
            action: v2::ExecutionAction::Continue as i32, ..Default::default() }).await.unwrap_err();
        assert!(matches!(error, ClientError::Api { error, .. } if error.code == v2::DdbErrorCode::FailedPrecondition as i32));
        let fresh = snapshot(&current).await;
        assert_ne!(fresh.server_instance_id, old_instance);
        assert!(fresh.breakpoints.is_empty());
        assert_eq!(fresh.stop_policy.unwrap().generation, 0);
        assert!(fresh.threads.iter().all(|thread| thread.state == v2::ThreadState::Stopped as i32));
    });
}

fn native_logpoint(backend: &str) {
    let _guard = support::real_test_guard();
    let example = support::build_real_loop_example();
    let binary = example.binary_path.to_string_lossy();
    let source = example.source_path.to_string_lossy();
    let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(
        backend,
        &[support::BinarySessionSpec {
            tag: "native-logpoint",
            alias: "native",
            hash: "native-policy",
            pid: 1741,
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path: &binary,
            binary_args: vec![
                "--sleep-ms".into(),
                "5".into(),
                "--max-iterations".into(),
                "100000".into(),
            ],
            stop_at_entry: true,
        }],
    );
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    runtime().block_on(two_client_logpoint(
        &ddb.api_endpoint(),
        &source,
        example.breakpoint_line as u32,
        true,
    ));
}

#[test]
fn gdb_temporary_logpoint_runs_in_the_backend_with_two_clients() {
    native_logpoint("gdb");
}

#[test]
fn lldb_temporary_logpoint_runs_in_the_backend_with_two_clients() {
    native_logpoint("lldb");
}
