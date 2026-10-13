use crate::{v2, ClientConfig, DdbClient, RequestTraceOutcome as Outcome};
use axum::{
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[tokio::test]
async fn retries_streams_and_clones_emit_only_canonical_metadata() {
    let calls = Arc::new(AtomicUsize::new(0));
    let router = Router::new().fallback(post(
        move |uri: Uri, headers: HeaderMap, Json(body): Json<serde_json::Value>| {
            let calls = calls.clone();
            async move {
                if uri.path().ends_with("/GetServerInfo") {
                    assert!(headers.get("authorization").is_none());
                } else {
                    assert_eq!(headers["authorization"], "Bearer secret-token");
                }
                if uri.path().ends_with("/Execute") {
                    assert_eq!(body["signalName"], "secret-expression");
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(v2::DdbError {
                                code: v2::DdbErrorCode::Unavailable as i32,
                                message: "secret-error-body".into(),
                                ..Default::default()
                            }),
                        )
                            .into_response();
                    }
                }
                if uri.path().ends_with("/SubscribeOutput") {
                    return "{\"text\":\"secret-output-body\"}\n".into_response();
                }
                Json(serde_json::json!({"unusedField": "secret-response-body"})).into_response()
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/secret-prefix?secret-query#secret-fragment",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client =
        DdbClient::new(ClientConfig::new(endpoint).with_bearer_token("secret-token")).unwrap();
    let mut traces = client.subscribe_request_traces();
    client
        .execute(v2::ExecuteRequest {
            signal_name: Some("secret-expression".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    drop(
        client
            .clone()
            .subscribe_output(v2::SubscribeOutputRequest::default())
            .await
            .unwrap(),
    );
    client
        .get_server_info(v2::GetServerInfoRequest::default())
        .await
        .unwrap();
    for (index, status) in [503, 200, 200, 200].into_iter().enumerate() {
        let start = traces.try_recv().unwrap();
        let end = traces.try_recv().unwrap();
        assert_eq!(start.attempt_id, index as u64 + 1);
        assert_eq!(start.attempt_id, end.attempt_id);
        assert_eq!(start.outcome, Outcome::Started);
        assert_eq!(end.outcome, Outcome::Response(status));
        assert_eq!(
            start.operation,
            ["Execute", "Execute", "SubscribeOutput", "GetServerInfo"][index]
        );
        assert!(!format!("{start:?} {end:?} {start} {end}").contains("secret"));
    }
    assert!(traces.try_recv().is_err());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cancellation_and_transport_timeout_have_fixed_safe_outcomes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    // The bound socket never sends HTTP headers. It supports both cancelling
    // an in-flight send and a deterministic request timeout without port races.
    let mut config = ClientConfig::new(format!(
        "http://{}/private-prefix",
        listener.local_addr().unwrap()
    ));
    config.request_timeout = Duration::from_millis(100);
    let client = DdbClient::new(config).unwrap();
    let mut traces = client.subscribe_request_traces();
    let copy = client.clone();
    let task = tokio::spawn(async move {
        copy.get_server_info(v2::GetServerInfoRequest::default())
            .await
    });
    let started = traces.recv().await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let cancelled = traces.try_recv().unwrap();
    assert_eq!(started.attempt_id, cancelled.attempt_id);
    assert_eq!(cancelled.outcome, Outcome::Cancelled);
    assert!(client
        .get_server_info(v2::GetServerInfoRequest::default())
        .await
        .is_err());
    assert_eq!(traces.try_recv().unwrap().outcome, Outcome::Started);
    let failed = traces.try_recv().unwrap();
    assert_eq!(failed.outcome, Outcome::TransportFailed);
    assert!(!format!("{failed:?} {failed}").contains("private-prefix"));
    drop(listener);
}
