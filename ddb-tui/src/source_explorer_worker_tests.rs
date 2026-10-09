use super::*;
use axum::{extract::State, routing::post, Json, Router};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

struct Backend {
    read_started: Semaphore,
    read_allowed: Semaphore,
    queries: Mutex<Vec<String>>,
}

async fn read(State(backend): State<Arc<Backend>>) -> Json<v2::ReadSourceResponse> {
    backend.read_started.add_permits(1);
    backend.read_allowed.acquire().await.unwrap().forget();
    Json(v2::ReadSourceResponse {
        context: Some(v2::ResponseContext {
            server_instance_id: "test".into(),
            ..Default::default()
        }),
        source: Some(v2::SourceContent {
            source: Some(v2::SourceFile {
                source_reference: "file".into(),
                ..Default::default()
            }),
            start_line: 1,
            content: "source".into(),
            line_count: 1,
            total_lines: 1,
            ..Default::default()
        }),
    })
}

async fn list(
    State(backend): State<Arc<Backend>>,
    Json(request): Json<v2::ListSourcesRequest>,
) -> Json<v2::ListSourcesResponse> {
    assert_eq!(request.page.as_ref().unwrap().page_size, 100);
    backend.queries.lock().unwrap().push(request.query);
    Json(v2::ListSourcesResponse {
        context: Some(v2::ResponseContext {
            server_instance_id: "test".into(),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[tokio::test]
async fn saturated_inspections_do_not_block_source_navigation() {
    tokio::time::timeout(Duration::from_secs(10), source_under_inspection_load(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn inspection_backlog_is_bounded_and_reports_overflow() {
    tokio::time::timeout(
        Duration::from_secs(10),
        source_under_inspection_load(MAX_PENDING_INSPECTIONS + 1),
    )
    .await
    .unwrap();
}

async fn source_under_inspection_load(queued: usize) {
    let started = Arc::new(Semaphore::new(0));
    let allowed = Arc::new(Semaphore::new(0));
    let backend = Arc::new(Backend {
        read_started: Semaphore::new(0),
        read_allowed: Semaphore::new(1),
        queries: Mutex::new(Vec::new()),
    });
    let router = Router::new()
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ReadSource",
            post(read),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListVariables",
            post({
                let started = started.clone();
                let allowed = allowed.clone();
                move || {
                    let started = started.clone();
                    let allowed = allowed.clone();
                    async move {
                        started.add_permits(1);
                        allowed.acquire().await.unwrap().forget();
                        Json(v2::ListVariablesResponse::default())
                    }
                }
            }),
        )
        .with_state(backend);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
    let (requests, receiver) = mpsc::channel(16);
    let (messages, mut updates) = mpsc::channel(16);
    let worker = tokio::spawn(run(client, Arc::default(), receiver, messages));
    for index in 0..4 {
        requests
            .send(BackendRequest::ExpandScope {
                scope_id: format!("scope/{index}"),
                thread_id: "thread".into(),
                generation: 1,
            })
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3), started.acquire_many(4))
        .await
        .unwrap()
        .unwrap()
        .forget();
    for index in 0..queued {
        requests
            .send(BackendRequest::ExpandScope {
                scope_id: format!("scope/queued/{index}"),
                thread_id: "thread".into(),
                generation: 1,
            })
            .await
            .unwrap();
    }
    if queued > MAX_PENDING_INSPECTIONS {
        let overflow = tokio::time::timeout(Duration::from_secs(2), updates.recv())
            .await
            .unwrap();
        assert!(matches!(overflow, Some(UiMessage::ExpansionError {
            generation: 1, thread_id, parent_id, error,
        }) if thread_id == "thread" && parent_id == format!("scope/queued/{MAX_PENDING_INSPECTIONS}") && error.contains("queue is full")));
    }
    let mut browser = crate::source_browser::SourceBrowser::default();
    let source = browser.open_source(crate::source_browser::tests::remote_source(
        v2::SourceFile {
            source_reference: "file".into(),
            ..Default::default()
        },
    ));
    requests
        .send(BackendRequest::LoadSource(source))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), updates.recv()).await;
    assert_eq!(
        started.available_permits(),
        0,
        "only four inspections may be active"
    );
    let expected = 4 + queued.min(MAX_PENDING_INSPECTIONS);
    allowed.add_permits(expected);
    let completed = tokio::time::timeout(Duration::from_secs(3), async {
        let mut parents = Vec::new();
        while parents.len() < expected {
            if let Some(UiMessage::VariableChildren { parent_id, .. }) = updates.recv().await {
                parents.push(parent_id);
            }
        }
        parents
    })
    .await;
    drop(requests);
    worker.await.unwrap();
    server.abort();
    let _ = server.await;
    assert!(
        matches!(result, Ok(Some(UiMessage::Source { .. }))),
        "source navigation must not wait for inspection capacity: {result:?}"
    );
    assert!(completed.unwrap().contains(&"scope/queued/0".to_string()));
}

#[tokio::test]
async fn discovery_is_debounced_and_cancellation_does_not_cancel_a_source_read() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let backend = Arc::new(Backend { read_started: Semaphore::new(0), read_allowed: Semaphore::new(0), queries: Mutex::new(Vec::new()) });
        let router = Router::new()
            .route("/api/v2/rpc/ddb.api.v2.DebuggerService/ReadSource", post(read))
            .route("/api/v2/rpc/ddb.api.v2.DebuggerService/ListSources", post(list))
            .with_state(backend.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
        let (requests, receiver) = mpsc::channel(8);
        let (messages, mut updates) = mpsc::channel(8);
        let worker = tokio::spawn(run(client, Arc::default(), receiver, messages));
        let mut browser = crate::source_browser::SourceBrowser::default();
        let source = browser.open_source(crate::source_browser::tests::remote_source(v2::SourceFile { source_reference: "file".into(), ..Default::default() }));
        requests.send(BackendRequest::LoadSource(source.clone())).await.unwrap();
        backend.read_started.acquire().await.unwrap().forget();

        let mut explorer = crate::source_explorer::Explorer::default();
        requests.send(BackendRequest::ExploreSources(explorer.open(true))).await.unwrap();
        let latest = explorer.edit_query(Some("main.rs")).unwrap();
        requests.send(BackendRequest::ExploreSources(latest.clone())).await.unwrap();
        assert!(matches!(updates.recv().await.unwrap(), UiMessage::SourceDiscovery { generation, result: Ok(_) } if generation == latest.generation));
        assert_eq!(*backend.queries.lock().unwrap(), ["main.rs"]);

        requests.send(BackendRequest::CancelSourceDiscovery).await.unwrap();
        backend.read_allowed.add_permits(1);
        assert!(matches!(updates.recv().await.unwrap(), UiMessage::Source { generation, .. } if generation == source.generation));
        drop(requests);
        worker.await.unwrap();
        server.abort();
        let _ = server.await;
    }).await.expect("independent source lanes should not deadlock");
}
