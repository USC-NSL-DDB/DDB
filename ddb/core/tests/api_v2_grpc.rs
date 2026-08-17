#![cfg(feature = "grpc-preview")]

mod support;

use std::time::{Duration, Instant};

use ddb_api_grpc::v2::{
    ddb_event_service_client::DdbEventServiceClient,
    debugger_control_service_client::DebuggerControlServiceClient,
    debugger_service_client::DebuggerServiceClient,
};
use ddb_api_types::v2::{
    target, DdbError, DdbErrorCode, ExecuteRequest, ExecutionAction, GetCapabilitiesRequest,
    GetOperationRequest, GetServerInfoRequest, GetSnapshotRequest, ListSessionsRequest,
    ListThreadsRequest, ReadMemoryRequest, RequestContext, ResourceKind, SessionTarget,
    StateEventFilter, StateEventKind, SubscribeStateEventsRequest, Target, ThreadTarget,
    TransportKind, WireEncoding,
};
use futures::stream;
use prost::Message;
use serde_json::json;
use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN};
use tonic::{
    metadata::MetadataValue,
    transport::{Channel, Endpoint},
    Code, Request, Status,
};
use tonic_health::pb::{health_check_response, health_client::HealthClient, HealthCheckRequest};
use tonic_reflection::pb::v1::{
    server_reflection_client::ServerReflectionClient, server_reflection_request::MessageRequest,
    ServerReflectionRequest,
};

const RPC_ROOT: &str = "/api/v2/rpc";
const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v2-grpc",
        alias: "native-frontend",
        hash: "api-v2-grpc-group",
        pid: 912,
        start_delay_ms: 0,
        source_file: "tests/api_v2_grpc.rs",
        source_line: 30,
        function: "mock_session",
        exit_on_continue: false,
    }
}

fn rpc(service: &str, method: &str) -> String {
    format!("{RPC_ROOT}/ddb.api.v2.{service}/{method}")
}

fn authorized<T>(message: T, token: &str) -> Request<T> {
    let mut request = Request::new(message);
    let authorization = MetadataValue::try_from(format!("Bearer {token}"))
        .expect("test bearer token should be valid metadata");
    request
        .metadata_mut()
        .insert("authorization", authorization);
    request
}

fn typed_error(status: &Status) -> DdbError {
    DdbError::decode(status.details()).expect("gRPC status details should contain DdbError")
}

async fn connect(endpoint: &str) -> Channel {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let channel = Endpoint::from_shared(endpoint.to_string())
            .expect("gRPC test endpoint should be valid")
            .connect()
            .await;
        match channel {
            Ok(channel) => return channel,
            Err(error) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
                let _ = error;
            }
            Err(error) => panic!("timed out connecting to gRPC preview endpoint: {error}"),
        }
    }
}

#[test]
fn grpc_preview_matches_http_semantics_and_enforces_the_v2_contract() {
    let mut ddb = DdbProcess::spawn_with_v2_auth_and_grpc(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let (http_status, http_sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert!(http_status.is_success(), "{http_sessions:?}");
    let http_session_id = http_sessions["sessions"][0]["sessionId"]
        .as_str()
        .expect("HTTP session should have an opaque id")
        .to_string();
    let http_display_name = http_sessions["sessions"][0]["displayName"]
        .as_str()
        .expect("HTTP session should have a display name")
        .to_string();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("test runtime should build");
    runtime.block_on(run_grpc_conformance(
        &ddb,
        &http_session_id,
        &http_display_name,
    ));
}

async fn run_grpc_conformance(ddb: &DdbProcess, http_session_id: &str, http_display_name: &str) {
    let channel = connect(&ddb.grpc_endpoint()).await;
    let mut debugger = DebuggerServiceClient::new(channel.clone());

    let info = debugger
        .get_server_info(Request::new(GetServerInfoRequest::default()))
        .await
        .expect("server identity should be public")
        .into_inner()
        .server_info
        .expect("server info should be populated");
    assert_eq!(info.name, "ddb");
    assert!(info.api_versions.iter().any(|version| version == "v2"));

    let unauthenticated = debugger
        .list_sessions(Request::new(ListSessionsRequest::default()))
        .await
        .expect_err("debugger state should require READ permission");
    assert_eq!(unauthenticated.code(), Code::Unauthenticated);
    let details = typed_error(&unauthenticated);
    assert_eq!(details.code, DdbErrorCode::Unauthenticated as i32);
    assert!(!details.request_id.is_empty());

    let capabilities = debugger
        .get_capabilities(authorized(
            GetCapabilitiesRequest::default(),
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("READ principal should discover capabilities")
        .into_inner()
        .capabilities
        .expect("capabilities should be populated");
    let grpc_endpoint = capabilities
        .transports
        .iter()
        .find(|endpoint| endpoint.transport == TransportKind::Grpc as i32)
        .expect("enabled gRPC preview should be discoverable");
    assert_eq!(
        grpc_endpoint.uri,
        ddb.grpc_endpoint().replacen("http://", "grpc://", 1)
    );
    assert_eq!(grpc_endpoint.encodings, vec![WireEncoding::Protobuf as i32]);
    assert!(!grpc_endpoint.tls_required);

    let grpc_sessions = debugger
        .list_sessions(authorized(
            ListSessionsRequest::default(),
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("gRPC session read should succeed")
        .into_inner()
        .sessions;
    assert_eq!(grpc_sessions.len(), 1);
    assert_eq!(http_session_id, grpc_sessions[0].session_id);
    assert_eq!(http_display_name, grpc_sessions[0].display_name);

    let session_target = Target {
        selector: Some(target::Selector::Session(SessionTarget {
            session_id: grpc_sessions[0].session_id.clone(),
        })),
    };
    let threads = debugger
        .list_threads(authorized(
            ListThreadsRequest {
                context: None,
                target: Some(session_target),
                page: None,
            },
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("gRPC thread read should succeed")
        .into_inner()
        .threads;
    let thread_id = threads
        .first()
        .expect("mock session should expose one thread")
        .thread_id
        .clone();
    let memory_request = ReadMemoryRequest {
        context: None,
        target: Some(Target {
            selector: Some(target::Selector::Thread(ThreadTarget {
                thread_id: thread_id.clone(),
            })),
        }),
        address: "0x1000".to_string(),
        byte_count: 8,
    };
    let denied_memory = debugger
        .read_memory(authorized(memory_request.clone(), V2_TEST_READ_TOKEN))
        .await
        .expect_err("READ principal must not inspect process memory");
    assert_eq!(denied_memory.code(), Code::PermissionDenied);
    assert_eq!(
        typed_error(&denied_memory).code,
        DdbErrorCode::PermissionDenied as i32
    );
    let memory = debugger
        .read_memory(authorized(memory_request, V2_TEST_CONTROL_TOKEN))
        .await
        .expect("CONTROL principal should inspect process memory")
        .into_inner()
        .memory
        .expect("memory response should contain a block");
    assert_eq!(memory.address, "0x1000");
    assert_eq!(memory.data.len(), 8);

    let snapshot_cursor = debugger
        .get_snapshot(authorized(
            GetSnapshotRequest::default(),
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("snapshot should succeed")
        .into_inner()
        .snapshot
        .and_then(|snapshot| snapshot.state_event_cursor)
        .expect("snapshot should carry a replay cursor");
    let mut events = DdbEventServiceClient::new(channel.clone())
        .subscribe_state_events(authorized(
            SubscribeStateEventsRequest {
                context: None,
                after_cursor: Some(snapshot_cursor),
                filter: Some(StateEventFilter {
                    kinds: vec![StateEventKind::OperationChanged as i32],
                    resource_kinds: vec![ResourceKind::Operation as i32],
                    session_ids: Vec::new(),
                    group_ids: Vec::new(),
                    include_extensions: false,
                }),
            },
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("state stream should accept a snapshot cursor")
        .into_inner();

    let execution_request = ExecuteRequest {
        context: Some(RequestContext {
            client_request_id: Some("grpc-conformance-next".to_string()),
            idempotency_key: Some("grpc-conformance-next-key".to_string()),
            deadline: None,
        }),
        target: Some(Target {
            selector: Some(target::Selector::Thread(ThreadTarget {
                thread_id: thread_id.clone(),
            })),
        }),
        action: ExecutionAction::Next as i32,
        jump_location: None,
        signal_name: None,
        preconditions: None,
    };
    let mut control = DebuggerControlServiceClient::new(channel.clone());
    let denied = control
        .execute(authorized(execution_request.clone(), V2_TEST_READ_TOKEN))
        .await
        .expect_err("READ principal must not admit debugger controls");
    assert_eq!(denied.code(), Code::PermissionDenied);
    assert_eq!(
        typed_error(&denied).code,
        DdbErrorCode::PermissionDenied as i32
    );

    let first = control
        .execute(authorized(execution_request.clone(), V2_TEST_CONTROL_TOKEN))
        .await
        .expect("CONTROL principal should admit execution")
        .into_inner()
        .operation
        .expect("admission should return an operation");
    let duplicate = control
        .execute(authorized(execution_request, V2_TEST_CONTROL_TOKEN))
        .await
        .expect("idempotent retry should return the original operation")
        .into_inner()
        .operation
        .expect("retry should return an operation");
    assert_eq!(duplicate.operation_id, first.operation_id);

    let event = tokio::time::timeout(WAIT_TIMEOUT, events.message())
        .await
        .expect("operation event should arrive before timeout")
        .expect("state stream should remain healthy")
        .expect("state stream should yield an event");
    assert_eq!(event.kind, StateEventKind::OperationChanged as i32);
    assert_eq!(event.resource_kind, ResourceKind::Operation as i32);
    assert_eq!(event.resource_id, first.operation_id);
    assert_eq!(
        event.operation_id.as_deref(),
        Some(first.operation_id.as_str())
    );

    let retained = debugger
        .get_operation(authorized(
            GetOperationRequest {
                context: None,
                operation_id: first.operation_id,
            },
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("admitted operation should be queryable")
        .into_inner()
        .operation
        .expect("operation should be present");
    assert_eq!(retained.request_id, first.request_id);

    let mut health = HealthClient::new(channel.clone());
    let unauthenticated_health = health
        .check(Request::new(HealthCheckRequest {
            service: "ddb.api.v2.DebuggerService".to_string(),
        }))
        .await
        .expect_err("standard health service should share READ authentication");
    assert_eq!(unauthenticated_health.code(), Code::Unauthenticated);
    let health = health
        .check(authorized(
            HealthCheckRequest {
                service: "ddb.api.v2.DebuggerService".to_string(),
            },
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("READ principal should access standard gRPC health")
        .into_inner();
    assert_eq!(
        health.status,
        health_check_response::ServingStatus::Serving as i32
    );

    let reflection_request = || ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    };
    let mut reflection = ServerReflectionClient::new(channel);
    let unauthenticated_reflection = reflection
        .server_reflection_info(Request::new(stream::iter([reflection_request()])))
        .await
        .expect_err("reflection should share READ authentication");
    assert_eq!(unauthenticated_reflection.code(), Code::Unauthenticated);
    let mut reflected = reflection
        .server_reflection_info(authorized(
            stream::iter([reflection_request()]),
            V2_TEST_READ_TOKEN,
        ))
        .await
        .expect("READ principal should access reflection")
        .into_inner();
    let reflected = reflected
        .message()
        .await
        .expect("reflection stream should remain healthy")
        .expect("reflection should return a response");
    let services = match reflected.message_response {
        Some(
            tonic_reflection::pb::v1::server_reflection_response::MessageResponse::ListServicesResponse(
                services,
            ),
        ) => services,
        other => panic!("expected reflected service list, got {other:?}"),
    };
    assert!(services
        .service
        .iter()
        .any(|service| service.name == "ddb.api.v2.DebuggerService"));
}
