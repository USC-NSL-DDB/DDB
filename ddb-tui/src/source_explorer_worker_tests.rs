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
        ..Default::default()
    })
}

async fn list(
    State(backend): State<Arc<Backend>>,
    Json(request): Json<v2::ListSourcesRequest>,
) -> Json<v2::ListSourcesResponse> {
    assert_eq!(request.page.as_ref().unwrap().page_size, 100);
    backend.queries.lock().unwrap().push(request.query);
    Json(v2::ListSourcesResponse::default())
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
        let worker = tokio::spawn(run(client, receiver, messages));
        let mut browser = crate::source_browser::SourceBrowser::default();
        let source = browser.navigate(None, v2::SourceLocation { source_reference: Some("file".into()), line: 1, ..Default::default() }, crate::source_browser::Navigation::Manual).unwrap();
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
