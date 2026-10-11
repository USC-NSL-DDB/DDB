use super::*;
use crate::{model::Control, operation_feedback::Update};
use axum::{routing::post, Json, Router};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

async fn next(rx: &mut mpsc::Receiver<UiMessage>) -> UiMessage {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn serialized_controls_report_admission_before_completion_with_distinct_intent_ids() {
    let admitted = Arc::new(AtomicUsize::new(0));
    let complete = Arc::new(tokio::sync::Semaphore::new(0));
    let router = Router::new()
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
            post({
                let admitted = admitted.clone();
                move |Json(request): Json<v2::ExecuteRequest>| {
                    let number = admitted.fetch_add(1, Ordering::SeqCst) + 1;
                    async move {
                        assert_eq!(
                            request.target,
                            Some(thread_target(format!("thread/{number}")))
                        );
                        Json(v2::OperationAdmissionResponse {
                            operation: Some(v2::Operation {
                                operation_id: format!("operation/{number}"),
                                state: v2::OperationState::Accepted as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                    }
                }
            }),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/GetOperation",
            post({
                let complete = complete.clone();
                move |Json(request): Json<v2::GetOperationRequest>| {
                    let complete = complete.clone();
                    async move {
                        complete.acquire().await.unwrap().forget();
                        Json(v2::GetOperationResponse {
                            operation: Some(v2::Operation {
                                state: if request.operation_id == "operation/1" {
                                    v2::OperationState::Completed
                                } else {
                                    v2::OperationState::Failed
                                } as i32,
                                operation_id: request.operation_id,
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ApiClient::new(crate::api::ClientConfig::new(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (tx, requests) = mpsc::channel(4);
    let (messages, mut rx) = mpsc::channel(16);
    let worker = tokio::spawn(run(client, Arc::default(), requests, messages));
    for ticket in [1, 2] {
        tx.send(BackendRequest::Tracked {
            ticket,
            request: Box::new(BackendRequest::Control(
                Control::Next,
                thread_target(format!("thread/{ticket}")),
            )),
        })
        .await
        .unwrap();
    }
    assert!(matches!(
        next(&mut rx).await,
        UiMessage::OperationFeedback {
            ticket: 1,
            update: Update::Started
        }
    ));
    assert!(
        matches!(next(&mut rx).await, UiMessage::OperationFeedback { ticket: 1, update: Update::Admitted(operation) } if operation.operation_id == "operation/1")
    );
    assert_eq!(
        admitted.load(Ordering::SeqCst),
        1,
        "second control must stay queued while the first is pending"
    );
    complete.add_permits(1);
    assert!(
        matches!(next(&mut rx).await, UiMessage::OperationFeedback { ticket: 1, update: Update::Finished(operation) } if operation.state == v2::OperationState::Completed as i32)
    );
    assert!(matches!(next(&mut rx).await, UiMessage::Receipt(_, _)));
    assert!(matches!(
        next(&mut rx).await,
        UiMessage::OperationFeedback {
            ticket: 2,
            update: Update::Started
        }
    ));
    assert!(
        matches!(next(&mut rx).await, UiMessage::OperationFeedback { ticket: 2, update: Update::Admitted(operation) } if operation.operation_id == "operation/2")
    );
    complete.add_permits(1);
    assert!(
        matches!(next(&mut rx).await, UiMessage::OperationFeedback { ticket: 2, update: Update::Finished(operation) } if operation.state == v2::OperationState::Failed as i32)
    );
    assert!(matches!(next(&mut rx).await, UiMessage::Receipt(_, _)));
    drop(tx);
    worker.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn rejected_admission_reports_the_exact_intent_error_without_a_success_receipt() {
    let router = Router::new().route(
        "/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
        post(|| async { axum::http::StatusCode::FORBIDDEN }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ApiClient::new(crate::api::ClientConfig::new(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (tx, requests) = mpsc::channel(4);
    let (messages, mut rx) = mpsc::channel(16);
    let worker = tokio::spawn(run(client, Arc::default(), requests, messages));
    tx.send(BackendRequest::Tracked {
        ticket: 9,
        request: Box::new(BackendRequest::Control(
            Control::Continue,
            thread_target("thread/a"),
        )),
    })
    .await
    .unwrap();
    assert!(matches!(
        next(&mut rx).await,
        UiMessage::OperationFeedback {
            ticket: 9,
            update: Update::Started
        }
    ));
    assert!(matches!(
        next(&mut rx).await,
        UiMessage::OperationFeedback {
            ticket: 9,
            update: Update::Error(_)
        }
    ));
    assert!(matches!(next(&mut rx).await, UiMessage::Error(_)));
    drop(tx);
    worker.await.unwrap();
    assert!(rx.recv().await.is_none());
    server.abort();
}
