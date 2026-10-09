use super::*;
use axum::{extract::State, routing::post, Json, Router};
use std::sync::Mutex;

struct Backend {
    case: &'static str,
    calls: Mutex<Vec<String>>,
    epoch: Arc<AtomicU64>,
    revision: AtomicU64,
}

fn context() -> Option<v2::ResponseContext> {
    Some(v2::ResponseContext {
        server_instance_id: "instance".into(),
        ..Default::default()
    })
}

fn stopped() -> v2::ExecutionState {
    v2::ExecutionState {
        execution_state_id: "execution/thread".into(),
        revision: 9,
        target: Some(crate::api::thread_target("thread/owner")),
        stop_reason: Some(v2::StopReason {
            kind: v2::StopReasonKind::Breakpoint as i32,
            thread_id: Some("thread/owner".into()),
            breakpoint_id: Some("breakpoint/log".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn frames(
    State(backend): State<Arc<Backend>>,
    Json(request): Json<v2::ListFramesRequest>,
) -> Json<v2::ListFramesResponse> {
    assert_eq!(request.thread_id, "thread/owner");
    assert_eq!(request.page.unwrap().page_size, 1);
    backend.calls.lock().unwrap().push("frames".into());
    Json(v2::ListFramesResponse {
        context: context(),
        frames: if backend.case == "missing_frame" {
            vec![]
        } else {
            vec![v2::Frame {
                frame_id: "frame/owner".into(),
                ..Default::default()
            }]
        },
        ..Default::default()
    })
}

async fn evaluate(
    State(backend): State<Arc<Backend>>,
    Json(request): Json<v2::EvaluateRequest>,
) -> Json<v2::OperationAdmissionResponse> {
    assert_eq!(request.target, stopped().target);
    assert_eq!(request.frame_id.as_deref(), Some("frame/owner"));
    assert_eq!(
        request.evaluation_context,
        v2::EvaluationContext::Watch as i32
    );
    backend
        .calls
        .lock()
        .unwrap()
        .push(format!("evaluate:{}", request.expression));
    if backend.case == "cancel_evaluation" {
        backend.epoch.store(2, Ordering::SeqCst);
    }
    Json(v2::OperationAdmissionResponse {
        context: context(),
        operation: Some(v2::Operation {
            state: if backend.case == "failed_evaluation" {
                v2::OperationState::Failed
            } else {
                v2::OperationState::Completed
            } as i32,
            result: (backend.case != "missing_value").then(|| v2::OperationResult {
                value: Some(v2::operation_result::Value::Evaluation(
                    v2::EvaluationResult {
                        value: if backend.case == "oversize" {
                            "x".repeat(MAX_TEMPLATE_BYTES + 1)
                        } else {
                            "42".into()
                        },
                        ..Default::default()
                    },
                )),
            }),
            ..Default::default()
        }),
    })
}

async fn state(
    State(backend): State<Arc<Backend>>,
    Json(request): Json<v2::GetExecutionStateRequest>,
) -> Json<v2::GetExecutionStateResponse> {
    assert_eq!(request.target, stopped().target);
    backend.calls.lock().unwrap().push("check".into());
    let mut latest = stopped();
    latest.revision = backend.revision.load(Ordering::SeqCst);
    match backend.case {
        "new_stop" => latest.revision += 1,
        "running" => latest.running = true,
        "wrong_target" => latest.target = Some(crate::api::thread_target("thread/other")),
        _ => {}
    }
    let mut context = context();
    if backend.case == "new_instance" {
        context.as_mut().unwrap().server_instance_id = "replacement".into();
    }
    Json(v2::GetExecutionStateResponse {
        context,
        execution_state: Some(latest),
    })
}

async fn execute(
    State(backend): State<Arc<Backend>>,
    Json(request): Json<v2::ExecuteRequest>,
) -> Json<v2::OperationAdmissionResponse> {
    assert_eq!(request.target, stopped().target);
    assert_eq!(request.action, v2::ExecutionAction::Continue as i32);
    backend.calls.lock().unwrap().push("continue".into());
    Json(v2::OperationAdmissionResponse {
        context: context(),
        operation: Some(v2::Operation {
            state: v2::OperationState::Completed as i32,
            ..Default::default()
        }),
    })
}

#[tokio::test]
async fn worker_applies_configuration_before_queued_stops_and_can_restore_stopping() {
    use crate::model::{BackendRequest, UiMessage};
    use std::time::Duration;
    let epoch = Arc::new(AtomicU64::new(1));
    let backend = Arc::new(Backend {
        case: "success",
        calls: Mutex::new(vec![]),
        epoch: epoch.clone(),
        revision: AtomicU64::new(9),
    });
    let router = Router::new()
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/GetSnapshot",
            post(|| async {
                Json(v2::GetSnapshotResponse {
                    context: context(),
                    snapshot: Some(v2::Snapshot {
                        server_instance_id: "instance".into(),
                        breakpoints: vec![v2::Breakpoint {
                            breakpoint_id: "breakpoint/log".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                })
            }),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListFrames",
            post(frames),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Evaluate",
            post(evaluate),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/GetExecutionState",
            post(state),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
            post(execute),
        )
        .with_state(backend.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = DdbClient::new(ddb_api_client::ClientConfig::new(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (requests, receiver) = tokio::sync::mpsc::channel(8);
    let (messages, mut updates) = tokio::sync::mpsc::channel(8);
    let worker = tokio::spawn(crate::worker::run(
        client,
        Arc::default(),
        receiver,
        messages,
    ));
    let make_stop = |revision| {
        let mut state = stopped();
        state.revision = revision;
        Stop {
            state,
            server_instance: "instance".into(),
            control_epoch: 1,
            current_epoch: epoch.clone(),
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        requests.send(BackendRequest::ConfigureLogpoint { id: "breakpoint/log".into(), instance: "instance".into(), template: Some(Template::parse("value {counter}").unwrap()) }).await.unwrap();
        // Queue the stop without waiting for the configuration acknowledgement.
        requests.send(BackendRequest::ClassifyStop(make_stop(9))).await.unwrap();
        requests.send(BackendRequest::ClassifyStop(make_stop(9))).await.unwrap();
        assert!(matches!(updates.recv().await, Some(UiMessage::LogpointsConfigured { ids, .. }) if ids == ["breakpoint/log"]));
        assert!(matches!(updates.recv().await, Some(UiMessage::LogpointOutput(text)) if text == "value 42\n"));
        loop {
            if backend.calls.lock().unwrap().iter().any(|call| call == "continue") { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        requests.send(BackendRequest::ConfigureLogpoint { id: "breakpoint/log".into(), instance: "instance".into(), template: None }).await.unwrap();
        assert!(matches!(updates.recv().await, Some(UiMessage::LogpointsConfigured { ids, .. }) if ids.is_empty()));
        requests.send(BackendRequest::ClassifyStop(make_stop(10))).await.unwrap();
        assert!(matches!(updates.recv().await, Some(UiMessage::ClassifiedStop { error: None, .. })));
        assert_eq!(backend.calls.lock().unwrap().iter().filter(|call| *call == "continue").count(), 1);
    }).await.unwrap();
    drop(requests);
    worker.await.unwrap();
    server.abort();
    let _ = server.await;
}

async fn exercise(
    case: &'static str,
    template: &str,
) -> (anyhow::Result<bool>, Vec<String>, Vec<String>) {
    let epoch = Arc::new(AtomicU64::new(if case == "cancel_before" { 2 } else { 1 }));
    let backend = Arc::new(Backend {
        case,
        epoch: epoch.clone(),
        calls: Mutex::new(vec![]),
        revision: AtomicU64::new(9),
    });
    let router = Router::new()
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListFrames",
            post(frames),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Evaluate",
            post(evaluate),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/GetExecutionState",
            post(state),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
            post(execute),
        )
        .with_state(backend.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = DdbClient::new(ddb_api_client::ClientConfig::new(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut stop = Stop {
        state: stopped(),
        server_instance: "instance".into(),
        control_epoch: 1,
        current_epoch: epoch.clone(),
    };
    if case == "peer_stop" {
        stop.state.stop_reason.as_mut().unwrap().thread_id = Some("thread/other".into());
    }
    let mut output = vec![];
    let result = run(
        &client,
        &Template::parse(template).unwrap(),
        &stop,
        |message| {
            if case == "failed_output" {
                return std::future::ready(Err(anyhow::anyhow!("output queue closed")));
            }
            output.push(message);
            if case == "cancel_output" {
                epoch.store(2, Ordering::SeqCst);
            }
            if case == "new_stop_during_output" {
                backend.revision.store(10, Ordering::SeqCst);
            }
            std::future::ready(Ok(()))
        },
    )
    .await;
    server.abort();
    let _ = server.await;
    let calls = backend.calls.lock().unwrap().clone();
    (result, calls, output)
}

#[tokio::test]
async fn interpolation_checks_the_exact_stop_then_outputs_and_continues() {
    let (result, calls, output) = exercise("success", "literal { counter }").await;
    assert!(result.unwrap());
    assert_eq!(
        calls,
        ["frames", "evaluate:counter", "check", "check", "continue"]
    );
    assert_eq!(output, ["literal 42\n"]);
    let (result, calls, output) = exercise("success", "literal {{braces}}\n").await;
    assert!(result.unwrap());
    assert_eq!(calls, ["check", "check", "continue"]);
    assert_eq!(output, ["literal {braces}\n"]);
}

#[tokio::test]
async fn superseded_logpoints_never_continue() {
    for case in [
        "cancel_before",
        "cancel_evaluation",
        "cancel_output",
        "new_stop_during_output",
        "new_stop",
        "running",
        "wrong_target",
    ] {
        let (result, calls, output) = exercise(case, "value {counter}").await;
        assert!(!result.unwrap(), "{case}");
        assert!(!calls.contains(&"continue".into()), "{case}: {calls:?}");
        if !matches!(case, "cancel_output" | "new_stop_during_output") {
            assert!(output.is_empty(), "{case}");
        }
    }
}

#[tokio::test]
async fn failed_or_oversize_logpoints_leave_the_thread_stopped() {
    for case in [
        "missing_frame",
        "missing_value",
        "failed_evaluation",
        "oversize",
        "new_instance",
        "peer_stop",
        "failed_output",
    ] {
        let (result, calls, output) = exercise(case, "value {counter}").await;
        assert!(result.is_err(), "{case}");
        assert!(!calls.contains(&"continue".into()), "{case}: {calls:?}");
        assert!(output.is_empty(), "{case}");
    }
    let (result, calls, _) = exercise("success", &"x".repeat(MAX_TEMPLATE_BYTES)).await;
    assert!(
        result.is_err(),
        "appended newline is part of the output budget"
    );
    assert!(!calls.contains(&"continue".into()));
}
