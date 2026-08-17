//! HTTP transport for DDB's command engine and read model.
//!
//! `/api/v1` is the stable client surface. The original unversioned routes
//! remain mounted as compatibility adapters and intentionally keep their
//! historical response shapes.

use std::{convert::Infallible, net::SocketAddr, path::Path as FsPath, sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    body::{Body, Bytes},
    extract::rejection::JsonRejection,
    extract::{DefaultBodyLimit, Extension, FromRef, Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use ddb_api_types::v2;
use futures::stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tracing::{debug, info};

use super::{
    application::{ApplicationError, DdbApplicationService, PrincipalContext},
    auth::{require_admin, require_control, require_read, ApiAuthorization},
    compatibility::CompatibilityCommandService,
    contract::{
        success, ApiError, ApiResult, BreakpointCreateRequest, BreakpointUpdateRequest,
        CommandReceipt, CommandRequest, DistributedBacktraceRequest, EvaluateRequest,
        ExecutionRequest, MemoryReadRequest, StackFramesRequest, StackVariablesRequest, Success,
        ThreadQueryRequest, ThreadSelectRequest, API_VERSION,
    },
    read_model::{ApiQueries, GroupView},
    security::{enforce_http_policy, HttpAdmissionPolicy},
    telemetry::observe_http,
};
use crate::{
    cmd_flow::{engine::CommandEngine, router::Target, FinishedCmd},
    common::Config,
    notification::{self, NotificationManager},
    shutdown::{ShutdownCause, ShutdownCtrl},
    state::BreakpointSnapshot,
    status::{Component, RuntimeStatus},
};

const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SOURCE_LINES: usize = 2_000;

#[derive(Deserialize, Debug, Clone)]
struct LegacySendCommand {
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    target: Option<Target>,
    cmd: String,
}

#[derive(Serialize)]
struct LegacySendCommandResponse {
    message: String,
    success: bool,
    payload: Option<FinishedCmd>,
}

#[derive(Deserialize, Debug)]
struct GetGroupQuery {
    grp_id: Option<u64>,
    grp_hash: Option<String>,
}

#[derive(Deserialize, Debug)]
struct LegacySourceQuery {
    src: String,
}

#[derive(Deserialize, Debug)]
struct SourceQuery {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Serialize)]
struct GroupIdsResponse {
    grp_ids: Vec<u64>,
}

#[derive(Serialize)]
struct GroupsResponse {
    grps: Vec<GroupView>,
}

#[derive(Serialize)]
struct BkptsResponse {
    bkpts: Vec<BreakpointSnapshot>,
}

#[derive(Serialize)]
struct LegacyApiResponse {
    message: String,
}

#[derive(Clone)]
struct ApiState {
    notifications: Arc<NotificationManager>,
    compatibility: Arc<CompatibilityCommandService>,
    queries: Arc<ApiQueries>,
    application: Arc<DdbApplicationService>,
    status: Arc<RuntimeStatus>,
    config: Arc<Config>,
    shutdown: Arc<ShutdownCtrl>,
}

impl FromRef<ApiState> for Arc<NotificationManager> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.notifications)
    }
}

impl FromRef<ApiState> for Arc<CompatibilityCommandService> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.compatibility)
    }
}

impl FromRef<ApiState> for Arc<ApiQueries> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.queries)
    }
}

impl FromRef<ApiState> for Arc<DdbApplicationService> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.application)
    }
}

impl FromRef<ApiState> for Arc<RuntimeStatus> {
    fn from_ref(state: &ApiState) -> Self {
        Arc::clone(&state.status)
    }
}
include!("generated/v2_contract.rs");

pub struct ApiServer {
    addr: SocketAddr,
    state: ApiState,
    authorization: Arc<ApiAuthorization>,
    admission: Arc<HttpAdmissionPolicy>,
}

impl ApiServer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        addr: SocketAddr,
        notifications: Arc<NotificationManager>,
        command_engine: Arc<CommandEngine>,
        queries: Arc<ApiQueries>,
        application: Arc<DdbApplicationService>,
        status: Arc<RuntimeStatus>,
        config: Arc<Config>,
        shutdown: Arc<ShutdownCtrl>,
        authorization: Arc<ApiAuthorization>,
    ) -> Result<Self> {
        let admission = Arc::new(HttpAdmissionPolicy::from_config(config.as_ref())?);
        let compatibility = CompatibilityCommandService::new(command_engine);
        Ok(Self {
            addr,
            state: ApiState {
                notifications,
                compatibility,
                queries,
                status,
                application,
                config,
                shutdown,
            },
            authorization,
            admission,
        })
    }

    fn router(&self) -> Router {
        let v2 = v2_contract_router(&self.authorization)
            .layer(DefaultBodyLimit::max(V2_MAX_REQUEST_BYTES));

        let v1 = Router::new()
            .route("/", get(service_info))
            .route("/capabilities", get(capabilities))
            .route("/health/live", get(liveness))
            .route("/health/ready", get(readiness))
            .route("/state", get(state_snapshot))
            .route("/sessions", get(v1_sessions))
            .route("/groups", get(v1_groups))
            .route("/breakpoints", get(v1_breakpoints).post(create_breakpoint))
            .route(
                "/breakpoints/:id",
                delete(delete_breakpoint).patch(update_breakpoint),
            )
            .route("/commands", post(execute_command))
            .route("/commands/pending", get(v1_pending_commands))
            .route("/execution", post(execute_control))
            .route("/threads/query", post(query_threads))
            .route("/threads/select", post(select_thread))
            .route("/stack/frames", post(stack_frames))
            .route("/stack/variables", post(stack_variables))
            .route("/evaluate", post(evaluate))
            .route("/memory/read", post(read_memory))
            .route("/sources/resolve", get(resolve_source))
            .route("/sources/content", get(source_content))
            .route("/ddb/distributed-backtrace", post(distributed_backtrace))
            .route("/events", get(notification::notification_subscribe_handler))
            .route("/shutdown", post(shutdown));

        let compatibility = Router::new()
            // The nested root owns `/api/v1`; Axum does not normalize its
            // trailing slash, so register only that alias explicitly.
            .route("/api/v1/", get(service_info))
            .nest("/api/v1", v1)
            // Compatibility surface. These are adapters over the same engine
            // and read model used by v1; they are not a second implementation.
            .route("/", get(legacy_root_handler))
            .route("/status", get(get_status))
            .route("/sessions", get(get_sessions))
            .route("/pcommands", get(get_pending_commands))
            .route("/src_to_grp_ids", get(resolve_src_to_group_ids))
            .route("/src_to_grps", get(resolve_src_to_groups))
            .route("/send", post(send_cmd))
            .route("/groups", get(get_groups))
            .route("/group", get(get_group))
            .route("/bkpts", get(get_bkpts))
            .route(
                "/notifications/subscribe",
                get(notification::notification_subscribe_handler),
            )
            .route(
                "/notifications/status",
                get(notification::notification_status_handler),
            )
            .route(
                "/notifications/test",
                post(notification::test_notification_handler),
            );

        // Remote listeners deliberately expose only the authenticated v2
        // contract. The unauthenticated v1 and historical compatibility
        // surfaces predate remote deployment and remain loopback-only.
        let mut router = Router::new().merge(v2);
        if self.addr.ip().is_loopback() {
            router = router.merge(compatibility);
        }
        router = router.fallback(api_not_found);
        if let Some(cors) = self.admission.cors_layer() {
            router = router.layer(cors);
        }
        router
            .layer(middleware::from_fn_with_state(
                Arc::clone(&self.admission),
                enforce_http_policy,
            ))
            .layer(middleware::from_fn(observe_http))
            .with_state(self.state.clone())
    }

    pub(crate) async fn bind(&self) -> Result<tokio::net::TcpListener, std::io::Error> {
        tokio::net::TcpListener::bind(self.addr).await
    }

    pub(crate) async fn run_listener(
        &self,
        listener: tokio::net::TcpListener,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), std::io::Error> {
        info!("[API Server]: Listening on {}", listener.local_addr()?);
        self.state.status.up(Component::Api);

        let application = Arc::clone(&self.state.application);
        let shutdown = async move {
            let _ = shutdown_rx.changed().await;
            // Close application-owned state/output subscriptions before Axum
            // waits for active streaming responses to drain. Doing this after
            // `serve` returns creates a circular wait with long-lived streams.
            application.shutdown();
        };

        let result = axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await;
        // Idempotent fallback for listener errors and early termination.
        self.state.application.shutdown();
        result?;
        Ok(())
    }
}

struct V2ApiError(ApplicationError);

impl From<ApplicationError> for V2ApiError {
    fn from(error: ApplicationError) -> Self {
        Self(error)
    }
}

impl IntoResponse for V2ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = v2_error_status(self.0.code());
        let request_id = uuid::Uuid::new_v4().to_string();
        (status, Json(self.0.to_contract(request_id))).into_response()
    }
}

fn v2_request<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, V2ApiError> {
    payload
        .map(|Json(request)| request)
        .map_err(|rejection| ApplicationError::invalid("body", rejection.body_text()).into())
}

fn v2_json_response<T: Serialize>(
    service: &DdbApplicationService,
    value: &T,
) -> Result<Response, V2ApiError> {
    let encoded = serde_json::to_vec(value).map_err(|_| {
        ApplicationError::new(v2::DdbErrorCode::Internal, "response serialization failed")
    })?;
    service.validate_response_bytes(encoded.len())?;

    let mut response = Response::new(Body::from(encoded));
    *response.status_mut() =
        StatusCode::from_u16(V2_SUCCESS_STATUS).expect("generated v2 success status is valid");
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(V2_UNARY_CONTENT_TYPE),
    );
    Ok(response)
}

macro_rules! v2_sync_handler {
    ($name:ident, $request:ty, $response:ty, $method:ident) => {
        async fn $name(
            State(service): State<Arc<DdbApplicationService>>,
            payload: Result<Json<$request>, JsonRejection>,
        ) -> Result<Response, V2ApiError> {
            let request = v2_request(payload)?;
            let response = service.$method(request)?;
            v2_json_response(service.as_ref(), &response)
        }
    };
}

macro_rules! v2_async_handler {
    ($name:ident, $request:ty, $response:ty, $method:ident) => {
        async fn $name(
            State(service): State<Arc<DdbApplicationService>>,
            payload: Result<Json<$request>, JsonRejection>,
        ) -> Result<Response, V2ApiError> {
            let request = v2_request(payload)?;
            let response = service.$method(request).await?;
            v2_json_response(service.as_ref(), &response)
        }
    };
}

macro_rules! v2_sync_principal_handler {
    ($name:ident, $request:ty, $response:ty, $method:ident) => {
        async fn $name(
            State(service): State<Arc<DdbApplicationService>>,
            Extension(principal): Extension<PrincipalContext>,
            payload: Result<Json<$request>, JsonRejection>,
        ) -> Result<Response, V2ApiError> {
            let request = v2_request(payload)?;
            let response = service.$method(&principal, request)?;
            v2_json_response(service.as_ref(), &response)
        }
    };
}

macro_rules! v2_async_principal_handler {
    ($name:ident, $request:ty, $response:ty, $method:ident) => {
        async fn $name(
            State(service): State<Arc<DdbApplicationService>>,
            Extension(principal): Extension<PrincipalContext>,
            payload: Result<Json<$request>, JsonRejection>,
        ) -> Result<Response, V2ApiError> {
            let request = v2_request(payload)?;
            let response = service.$method(&principal, request).await?;
            v2_json_response(service.as_ref(), &response)
        }
    };
}

async fn v2_subscribe_state_events(
    State(service): State<Arc<DdbApplicationService>>,
    payload: Result<Json<v2::SubscribeStateEventsRequest>, JsonRejection>,
) -> Result<Response, V2ApiError> {
    let request = v2_request(payload)?;
    let subscription = service.subscribe_state_events(request)?;
    let stream = stream::unfold(subscription, |mut subscription| async move {
        match tokio::time::timeout(
            V2_DDB_EVENT_SERVICE_SUBSCRIBE_STATE_EVENTS_HEARTBEAT,
            subscription.recv(),
        )
        .await
        {
            Ok(Some(event)) => {
                let mut encoded = serde_json::to_vec(&event)
                    .expect("generated state events must be ProtoJSON serializable");
                encoded.push(b'\n');
                Some((Ok::<Bytes, Infallible>(Bytes::from(encoded)), subscription))
            }
            Ok(None) => None,
            Err(_) => Some((
                Ok::<Bytes, Infallible>(Bytes::from_static(b"\n")),
                subscription,
            )),
        }
    });
    let mut response = Body::from_stream(stream).into_response();
    *response.status_mut() =
        StatusCode::from_u16(V2_SUCCESS_STATUS).expect("generated v2 success status is valid");
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(V2_STREAM_CONTENT_TYPE),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(response)
}

async fn v2_subscribe_output(
    State(service): State<Arc<DdbApplicationService>>,
    payload: Result<Json<v2::SubscribeOutputRequest>, JsonRejection>,
) -> Result<Response, V2ApiError> {
    let request = v2_request(payload)?;
    let subscription = service.subscribe_output(request)?;
    let stream = stream::unfold(subscription, |mut subscription| async move {
        match tokio::time::timeout(
            V2_DDB_EVENT_SERVICE_SUBSCRIBE_OUTPUT_HEARTBEAT,
            subscription.recv(),
        )
        .await
        {
            Ok(Some(event)) => {
                let mut encoded = serde_json::to_vec(&event)
                    .expect("generated output events must be ProtoJSON serializable");
                encoded.push(b'\n');
                Some((Ok::<Bytes, Infallible>(Bytes::from(encoded)), subscription))
            }
            Ok(None) => None,
            Err(_) => Some((
                Ok::<Bytes, Infallible>(Bytes::from_static(b"\n")),
                subscription,
            )),
        }
    });
    let mut response = Body::from_stream(stream).into_response();
    *response.status_mut() =
        StatusCode::from_u16(V2_SUCCESS_STATUS).expect("generated v2 success status is valid");
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(V2_STREAM_CONTENT_TYPE),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(response)
}

v2_sync_handler!(
    v2_get_server_info,
    v2::GetServerInfoRequest,
    v2::GetServerInfoResponse,
    get_server_info
);
v2_async_handler!(
    v2_get_capabilities,
    v2::GetCapabilitiesRequest,
    v2::GetCapabilitiesResponse,
    get_capabilities
);
v2_async_handler!(
    v2_get_snapshot,
    v2::GetSnapshotRequest,
    v2::GetSnapshotResponse,
    get_snapshot
);
v2_async_handler!(
    v2_list_sessions,
    v2::ListSessionsRequest,
    v2::ListSessionsResponse,
    list_sessions
);
v2_async_handler!(
    v2_get_session,
    v2::GetSessionRequest,
    v2::GetSessionResponse,
    get_session
);
v2_async_handler!(
    v2_list_processes,
    v2::ListProcessesRequest,
    v2::ListProcessesResponse,
    list_processes
);
v2_async_handler!(
    v2_get_process,
    v2::GetProcessRequest,
    v2::GetProcessResponse,
    get_process
);
v2_async_handler!(
    v2_list_threads,
    v2::ListThreadsRequest,
    v2::ListThreadsResponse,
    list_threads
);
v2_async_handler!(
    v2_get_thread,
    v2::GetThreadRequest,
    v2::GetThreadResponse,
    get_thread
);
v2_async_handler!(
    v2_list_frames,
    v2::ListFramesRequest,
    v2::ListFramesResponse,
    list_frames
);
v2_async_handler!(
    v2_get_execution_state,
    v2::GetExecutionStateRequest,
    v2::GetExecutionStateResponse,
    get_execution_state
);
v2_async_handler!(
    v2_list_scopes,
    v2::ListScopesRequest,
    v2::ListScopesResponse,
    list_scopes
);
v2_async_handler!(
    v2_list_variables,
    v2::ListVariablesRequest,
    v2::ListVariablesResponse,
    list_variables
);
v2_async_handler!(
    v2_expand_variable,
    v2::ExpandVariableRequest,
    v2::ExpandVariableResponse,
    expand_variable
);
v2_async_handler!(
    v2_list_registers,
    v2::ListRegistersRequest,
    v2::ListRegistersResponse,
    list_registers
);
v2_async_handler!(
    v2_list_signals,
    v2::ListSignalsRequest,
    v2::ListSignalsResponse,
    list_signals
);
v2_async_handler!(
    v2_read_memory,
    v2::ReadMemoryRequest,
    v2::ReadMemoryResponse,
    read_memory
);
v2_async_handler!(
    v2_resolve_source,
    v2::ResolveSourceRequest,
    v2::ResolveSourceResponse,
    resolve_source
);
v2_async_handler!(
    v2_read_source,
    v2::ReadSourceRequest,
    v2::ReadSourceResponse,
    read_source
);
v2_async_handler!(
    v2_list_groups,
    v2::ListGroupsRequest,
    v2::ListGroupsResponse,
    list_groups
);
v2_async_handler!(
    v2_get_group,
    v2::GetGroupRequest,
    v2::GetGroupResponse,
    get_group
);
v2_async_handler!(
    v2_list_breakpoints,
    v2::ListBreakpointsRequest,
    v2::ListBreakpointsResponse,
    list_breakpoints
);
v2_sync_handler!(
    v2_get_breakpoint,
    v2::GetBreakpointRequest,
    v2::GetBreakpointResponse,
    get_breakpoint
);
v2_async_handler!(
    v2_list_pending_commands,
    v2::ListPendingCommandsRequest,
    v2::ListPendingCommandsResponse,
    list_pending_commands
);
v2_sync_handler!(
    v2_get_operation,
    v2::GetOperationRequest,
    v2::GetOperationResponse,
    get_operation
);
v2_async_handler!(
    v2_list_operations,
    v2::ListOperationsRequest,
    v2::ListOperationsResponse,
    list_operations
);
v2_sync_handler!(
    v2_list_extension_states,
    v2::ListExtensionStatesRequest,
    v2::ListExtensionStatesResponse,
    list_extension_states
);
v2_sync_handler!(
    v2_get_extension_schema,
    v2::GetExtensionSchemaRequest,
    v2::GetExtensionSchemaResponse,
    get_extension_schema
);
v2_sync_handler!(
    v2_get_health,
    v2::GetHealthRequest,
    v2::GetHealthResponse,
    get_health
);
v2_sync_handler!(
    v2_get_readiness,
    v2::GetReadinessRequest,
    v2::GetReadinessResponse,
    get_readiness
);
v2_async_principal_handler!(
    v2_execute,
    v2::ExecuteRequest,
    v2::OperationAdmissionResponse,
    execute
);
v2_async_principal_handler!(
    v2_select_thread,
    v2::SelectThreadRequest,
    v2::OperationAdmissionResponse,
    select_thread
);
v2_async_principal_handler!(
    v2_evaluate,
    v2::EvaluateRequest,
    v2::OperationAdmissionResponse,
    evaluate
);
v2_async_principal_handler!(
    v2_create_breakpoint,
    v2::CreateBreakpointRequest,
    v2::OperationAdmissionResponse,
    create_breakpoint
);
v2_async_principal_handler!(
    v2_update_breakpoint,
    v2::UpdateBreakpointRequest,
    v2::OperationAdmissionResponse,
    update_breakpoint
);
v2_async_principal_handler!(
    v2_delete_breakpoint,
    v2::DeleteBreakpointRequest,
    v2::OperationAdmissionResponse,
    delete_breakpoint
);
v2_async_principal_handler!(
    v2_execute_raw_command,
    v2::ExecuteRawCommandRequest,
    v2::OperationAdmissionResponse,
    execute_raw_command
);
v2_async_principal_handler!(
    v2_run_distributed_backtrace,
    v2::RunDistributedBacktraceRequest,
    v2::OperationAdmissionResponse,
    run_distributed_backtrace
);
v2_async_principal_handler!(
    v2_invoke_extension_action,
    v2::InvokeExtensionActionRequest,
    v2::OperationAdmissionResponse,
    invoke_extension_action
);
v2_sync_principal_handler!(
    v2_cancel_operation,
    v2::CancelOperationRequest,
    v2::OperationAdmissionResponse,
    cancel_operation
);
v2_sync_principal_handler!(
    v2_shutdown,
    v2::ShutdownRequest,
    v2::OperationAdmissionResponse,
    shutdown_request
);

fn enum_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

async fn service_info(State(state): State<ApiState>) -> ApiResult<JsonValue> {
    Ok(success(json!({
        "name": "ddb",
        "api_version": API_VERSION,
        "ddb_version": env!("CARGO_PKG_VERSION"),
        "backend": enum_name(&state.config.conf.debugger.backend),
        "framework": enum_name(&state.config.framework),
        "documentation": "/api/v1/capabilities",
        "event_stream": "/api/v1/events"
    })))
}

async fn capabilities(State(state): State<ApiState>) -> ApiResult<JsonValue> {
    let backend = enum_name(&state.config.conf.debugger.backend);
    let extensions = state.queries.legacy_extension_descriptors();
    Ok(success(json!({
        "protocol": {
            "version": API_VERSION,
            "transports": ["http", "websocket"],
            "generic_command_passthrough": true,
            "legacy_routes": true
        },
        "runtime": {
            "backend": backend,
            "framework": enum_name(&state.config.framework),
            "migration": state.config.conf.support_migration
        },
        "resources": ["state", "sessions", "groups", "threads", "breakpoints", "sources", "pending_commands"],
        "breakpoint_actions": ["create", "delete", "enable", "disable", "conditional", "temporary", "hardware"],
        "execution_actions": ["continue", "interrupt", "next", "step_in", "step_out", "jump", "send_signal"],
        "inspection": ["stack_frames", "stack_variables", "evaluate", "memory", "source_content"],
        "ddb_features": ["distributed_backtrace", "multi_target_routing", "group_breakpoints", "context_restore"],
        "extensions": extensions,
        "target_kinds": ["session", "thread", "group", "current_thread", "current_session", "session_set", "broadcast", "first", "multiple"],
        "events": ["debugger_output", "breakpoint_changed", "session_status_changed", "session_list_changed", "custom"]
    })))
}

async fn liveness() -> ApiResult<JsonValue> {
    Ok(success(json!({"status": "up"})))
}

async fn readiness(State(status): State<Arc<RuntimeStatus>>) -> impl IntoResponse {
    if status.is_up() {
        (StatusCode::OK, success(json!({"status": "ready"}))).into_response()
    } else {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "one or more DDB runtime components are not ready",
        )
        .into_response()
    }
}

async fn state_snapshot(State(queries): State<Arc<ApiQueries>>) -> ApiResult<JsonValue> {
    let snapshot = queries.snapshot().await;
    let legacy_extensions = queries.legacy_extension_states(&snapshot.extensions);
    let mut snapshot = serde_json::to_value(snapshot)
        .map_err(|error| ApiError::internal("serialization_failed", error.to_string()))?;
    snapshot["extensions"] = serde_json::to_value(legacy_extensions)
        .map_err(|error| ApiError::internal("serialization_failed", error.to_string()))?;
    Ok(success(snapshot))
}

async fn v1_sessions(State(queries): State<Arc<ApiQueries>>) -> ApiResult<JsonValue> {
    Ok(success(json!({"items": queries.sessions().await})))
}

async fn v1_groups(State(queries): State<Arc<ApiQueries>>) -> ApiResult<JsonValue> {
    Ok(success(json!({"items": queries.groups()})))
}

async fn v1_breakpoints(State(queries): State<Arc<ApiQueries>>) -> ApiResult<JsonValue> {
    Ok(success(json!({"items": queries.breakpoints()})))
}

async fn v1_pending_commands(State(queries): State<Arc<ApiQueries>>) -> ApiResult<JsonValue> {
    Ok(success(json!({"items": queries.pending_commands()})))
}

async fn execute_command(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<CommandRequest>,
) -> Result<(StatusCode, Json<Success<CommandReceipt>>), ApiError> {
    let response = service.execute_command(request).await?;
    let status = if response.accepted {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, success(response.receipt)))
}

async fn execute_control(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<ExecutionRequest>,
) -> ApiResult<CommandReceipt> {
    service.execute_control(request).await.map(success)
}

async fn query_threads(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<ThreadQueryRequest>,
) -> ApiResult<CommandReceipt> {
    service.query_threads(request).await.map(success)
}

async fn select_thread(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<ThreadSelectRequest>,
) -> ApiResult<CommandReceipt> {
    service.select_thread(request).await.map(success)
}

async fn stack_frames(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<StackFramesRequest>,
) -> ApiResult<CommandReceipt> {
    service.stack_frames(request).await.map(success)
}

async fn stack_variables(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<StackVariablesRequest>,
) -> ApiResult<CommandReceipt> {
    service.stack_variables(request).await.map(success)
}

async fn evaluate(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<EvaluateRequest>,
) -> ApiResult<CommandReceipt> {
    service.evaluate(request).await.map(success)
}

async fn read_memory(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<MemoryReadRequest>,
) -> ApiResult<CommandReceipt> {
    service.read_memory(request).await.map(success)
}

async fn create_breakpoint(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<BreakpointCreateRequest>,
) -> ApiResult<CommandReceipt> {
    service.create_breakpoint(request).await.map(success)
}

async fn delete_breakpoint(
    State(service): State<Arc<CompatibilityCommandService>>,
    Path(id): Path<u64>,
) -> ApiResult<CommandReceipt> {
    service.delete_breakpoint(id).await.map(success)
}

async fn update_breakpoint(
    State(service): State<Arc<CompatibilityCommandService>>,
    Path(id): Path<u64>,
    Json(request): Json<BreakpointUpdateRequest>,
) -> ApiResult<CommandReceipt> {
    service.update_breakpoint(id, request).await.map(success)
}

async fn distributed_backtrace(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(request): Json<DistributedBacktraceRequest>,
) -> ApiResult<CommandReceipt> {
    service.distributed_backtrace(request).await.map(success)
}

async fn resolve_source(
    State(queries): State<Arc<ApiQueries>>,
    Query(query): Query<SourceQuery>,
) -> ApiResult<JsonValue> {
    let group_ids = queries
        .group_ids_for_source(&query.path)
        .await
        .map_err(|error| {
            ApiError::unprocessable("source_resolution_failed", format!("{error:#}"))
        })?;
    let groups = queries
        .groups_for_source(&query.path)
        .await
        .map_err(|error| {
            ApiError::unprocessable("source_resolution_failed", format!("{error:#}"))
        })?;
    Ok(success(json!({
        "path": query.path,
        "group_ids": group_ids,
        "groups": groups
    })))
}

#[derive(Serialize)]
struct SourceContent {
    path: String,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    lines: Vec<String>,
}

async fn source_content(
    State(queries): State<Arc<ApiQueries>>,
    Query(query): Query<SourceQuery>,
) -> ApiResult<SourceContent> {
    let groups = queries
        .group_ids_for_source(&query.path)
        .await
        .map_err(|error| {
            ApiError::unprocessable("source_resolution_failed", format!("{error:#}"))
        })?;
    if groups.is_empty() {
        return Err(ApiError::not_found(
            "unknown_source",
            format!("the debugger did not report source '{}'", query.path),
        ));
    }

    let metadata = tokio::fs::metadata(&query.path)
        .await
        .map_err(|error| ApiError::not_found("source_unavailable", error.to_string()))?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return Err(ApiError::bad_request(
            "source_too_large",
            format!("source files must be regular files no larger than {MAX_SOURCE_BYTES} bytes"),
        ));
    }
    let contents = tokio::fs::read_to_string(&query.path)
        .await
        .map_err(|error| ApiError::unprocessable("source_unreadable", error.to_string()))?;
    let all_lines = contents.lines().map(str::to_string).collect::<Vec<_>>();
    let total_lines = all_lines.len();
    let start_line = query.start_line.unwrap_or(1).max(1);
    let requested_end = query
        .end_line
        .unwrap_or_else(|| start_line.saturating_add(399));
    if requested_end < start_line {
        return Err(ApiError::bad_request(
            "invalid_line_range",
            "end_line must be greater than or equal to start_line",
        ));
    }
    let end_line = requested_end
        .min(start_line.saturating_add(MAX_SOURCE_LINES - 1))
        .min(total_lines);
    let lines = if start_line > total_lines {
        Vec::new()
    } else {
        all_lines[start_line - 1..end_line].to_vec()
    };
    let path = FsPath::new(&query.path)
        .canonicalize()
        .unwrap_or_else(|_| query.path.clone().into())
        .to_string_lossy()
        .to_string();
    Ok(success(SourceContent {
        path,
        start_line,
        end_line,
        total_lines,
        lines,
    }))
}

async fn shutdown(State(state): State<ApiState>) -> ApiResult<JsonValue> {
    let shutdown = Arc::clone(&state.shutdown);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.trigger_once(ShutdownCause::UserExit);
    });
    Ok(success(json!({"state": "shutting_down"})))
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("route_not_found", "the requested API route does not exist")
}

// ---- Legacy compatibility handlers ---------------------------------------------------------

async fn legacy_root_handler() -> Json<LegacyApiResponse> {
    Json(LegacyApiResponse {
        message: "DDB API server. Use /api/v1 for the versioned interface.".to_string(),
    })
}

fn source_resolution_error(error: anyhow::Error) -> (StatusCode, Json<LegacyApiResponse>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(LegacyApiResponse {
            message: format!("Failed to resolve debugger sources: {error:#}"),
        }),
    )
}

async fn resolve_src_to_group_ids(
    State(queries): State<Arc<ApiQueries>>,
    Query(src): Query<LegacySourceQuery>,
) -> std::result::Result<Json<GroupIdsResponse>, (StatusCode, Json<LegacyApiResponse>)> {
    let grp_ids = queries
        .group_ids_for_source(&src.src)
        .await
        .map_err(source_resolution_error)?;
    Ok(Json(GroupIdsResponse { grp_ids }))
}

async fn resolve_src_to_groups(
    State(queries): State<Arc<ApiQueries>>,
    Query(src): Query<LegacySourceQuery>,
) -> std::result::Result<Json<GroupsResponse>, (StatusCode, Json<LegacyApiResponse>)> {
    let grps = queries
        .groups_for_source(&src.src)
        .await
        .map_err(source_resolution_error)?;
    Ok(Json(GroupsResponse { grps }))
}

async fn send_cmd(
    State(service): State<Arc<CompatibilityCommandService>>,
    Json(send_cmd): Json<LegacySendCommand>,
) -> impl IntoResponse {
    debug!(
        wait = send_cmd.wait,
        target = ?send_cmd.target,
        command_bytes = send_cmd.cmd.len(),
        "received legacy command"
    );
    let result: Result<Option<FinishedCmd>> = service
        .execute_legacy(&send_cmd.cmd, send_cmd.target, send_cmd.wait)
        .await;

    match result {
        Ok(payload) => (
            StatusCode::OK,
            Json(LegacySendCommandResponse {
                message: "success".to_string(),
                success: true,
                payload,
            }),
        ),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(LegacySendCommandResponse {
                message: format!("Failed to process command: {error}"),
                success: false,
                payload: None,
            }),
        ),
    }
}

async fn get_status(State(status): State<Arc<RuntimeStatus>>) -> impl IntoResponse {
    if status.is_up() {
        (StatusCode::OK, Json(json!({"status": "up"})))
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "down"})),
        )
    }
}

async fn get_sessions(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (StatusCode::OK, Json(queries.sessions().await))
}

async fn get_pending_commands(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (StatusCode::OK, Json(queries.pending_commands()))
}

async fn get_groups(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (StatusCode::OK, Json(queries.groups()))
}

async fn get_group(
    State(queries): State<Arc<ApiQueries>>,
    Query(query): Query<GetGroupQuery>,
) -> impl IntoResponse {
    if let Some(grp_id) = query.grp_id {
        if let Some(group_meta) = queries.group_by_id(grp_id) {
            (StatusCode::OK, Json(json!(group_meta)))
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Group not found"})),
            )
        }
    } else if let Some(grp_hash) = query.grp_hash {
        if let Some(group_meta) = queries.group_by_hash(&grp_hash) {
            (StatusCode::OK, Json(json!(group_meta)))
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Group not found"})),
            )
        }
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Either grp_id or grp_hash must be provided"})),
        )
    }
}

async fn get_bkpts(State(queries): State<Arc<ApiQueries>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(BkptsResponse {
            bkpts: queries.breakpoints(),
        }),
    )
}
