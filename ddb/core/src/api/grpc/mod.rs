//! Optional native gRPC binding for the public v2 application service.
//!
//! This module owns transport framing, authorization, status mapping, and
//! server lifecycle only. Debugger semantics remain in
//! `DdbApplicationService`, which is shared with the HTTP binding.

use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use ddb_api_grpc::v2::{
    ddb_admin_service_server::{DdbAdminService, DdbAdminServiceServer},
    ddb_event_service_server::{DdbEventService, DdbEventServiceServer},
    debugger_control_service_server::{DebuggerControlService, DebuggerControlServiceServer},
    debugger_service_server::{DebuggerService, DebuggerServiceServer},
};
use ddb_api_types::v2;
use futures::{stream, Stream};
use prost::Message;
use tonic::{
    service::interceptor::InterceptedService, transport::Server, Code, Request, Response, Status,
};
use tracing::info;

use super::{
    application::{ApplicationError, DdbApplicationService, PrincipalContext},
    auth::ApiAuthorization,
    telemetry::{attach_grpc_trace_parent, record_authorization, record_grpc_request},
};

const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONCURRENT_STREAMS_PER_CONNECTION: u32 = 256;
const MAX_CONCURRENT_REQUESTS_PER_CONNECTION: usize = 256;
const MAX_HEADER_BYTES: u32 = 16 * 1024;

type GrpcResult<T> = Result<Response<T>, Status>;
type StateEventStream =
    Pin<Box<dyn Stream<Item = Result<v2::StateEvent, Status>> + Send + 'static>>;
type OutputEventStream =
    Pin<Box<dyn Stream<Item = Result<v2::OutputEvent, Status>> + Send + 'static>>;

fn observe_unary<T: Message>(
    method: &'static str,
    started: Instant,
    request_bytes: usize,
    result: &GrpcResult<T>,
) {
    let status = result
        .as_ref()
        .map_or_else(|error| error.code(), |_| Code::Ok);
    let response_bytes = result
        .as_ref()
        .ok()
        .map(|response| response.get_ref().encoded_len());
    record_grpc_request(method, started, request_bytes, response_bytes, status);
}

fn observe_stream<T>(
    method: &'static str,
    started: Instant,
    request_bytes: usize,
    result: &Result<Response<T>, Status>,
) {
    let status = result
        .as_ref()
        .map_or_else(|error| error.code(), |_| Code::Ok);
    record_grpc_request(method, started, request_bytes, None, status);
}

#[derive(Clone)]
struct GrpcApi {
    application: Arc<DdbApplicationService>,
    authorization: Arc<ApiAuthorization>,
}

impl GrpcApi {
    fn new(application: Arc<DdbApplicationService>, authorization: Arc<ApiAuthorization>) -> Self {
        Self {
            application,
            authorization,
        }
    }

    fn authorize<T>(
        &self,
        request: &Request<T>,
        required: v2::PermissionScope,
    ) -> Result<PrincipalContext, Status> {
        attach_grpc_trace_parent(request);
        let method = request
            .extensions()
            .get::<tonic::GrpcMethod<'static>>()
            .map(|method| format!("{}.{}", method.service(), method.method()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let authorization = authorization_metadata(request);
        match self
            .authorization
            .authenticate_authorization(authorization, required)
        {
            Ok(principal) => {
                record_authorization("grpc", &method, required, "allowed", Some(principal.id()));
                Ok(principal)
            }
            Err(error) => {
                record_authorization("grpc", &method, required, "denied", None);
                Err(grpc_status(error))
            }
        }
    }
}

macro_rules! implement_grpc_api {
    (
        public_sync { $( $public_name:ident: $public_request:ty => $public_response:ty, )* }
        read_sync { $( $read_sync_name:ident: $read_sync_request:ty => $read_sync_response:ty, )* }
        read_async { $( $read_async_name:ident: $read_async_request:ty => $read_async_response:ty, )* }
        sensitive_read_async { $( $sensitive_read_name:ident: $sensitive_read_request:ty => $sensitive_read_response:ty, )* }
        control_sync { $( $control_sync_name:ident: $control_sync_request:ty, )* }
        control_async { $( $control_async_name:ident: $control_async_request:ty, )* }
    ) => {
        #[tonic::async_trait]
        impl DebuggerService for GrpcApi {
            $(
                async fn $public_name(
                    &self,
                    request: Request<$public_request>,
                ) -> GrpcResult<$public_response> {
                    let started = Instant::now();
                    let request_bytes = request.get_ref().encoded_len();
                    attach_grpc_trace_parent(&request);
                    let result = self.application
                        .$public_name(request.into_inner())
                        .map(Response::new)
                        .map_err(grpc_status);
                    observe_unary(
                        concat!("ddb.api.v2.DebuggerService/", stringify!($public_name)),
                        started,
                        request_bytes,
                        &result,
                    );
                    result
                }
            )*
            $(
                async fn $read_sync_name(
                    &self,
                    request: Request<$read_sync_request>,
                ) -> GrpcResult<$read_sync_response> {
                    let started = Instant::now();
                    let request_bytes = request.get_ref().encoded_len();
                    let result = match self.authorize(&request, v2::PermissionScope::Read) {
                        Ok(_) => self.application
                            .$read_sync_name(request.into_inner())
                            .map(Response::new)
                            .map_err(grpc_status),
                        Err(error) => Err(error),
                    };
                    observe_unary(
                        concat!("ddb.api.v2.DebuggerService/", stringify!($read_sync_name)),
                        started,
                        request_bytes,
                        &result,
                    );
                    result
                }
            )*
            $(
                async fn $read_async_name(
                    &self,
                    request: Request<$read_async_request>,
                ) -> GrpcResult<$read_async_response> {
                    let started = Instant::now();
                    let request_bytes = request.get_ref().encoded_len();
                    let result = match self.authorize(&request, v2::PermissionScope::Read) {
                        Ok(_) => self.application
                            .$read_async_name(request.into_inner())
                            .await
                            .map(Response::new)
                            .map_err(grpc_status),
                        Err(error) => Err(error),
                    };
                    observe_unary(
                        concat!("ddb.api.v2.DebuggerService/", stringify!($read_async_name)),
                        started,
                        request_bytes,
                        &result,
                    );
                    result
                }
            )*
            $(
                async fn $sensitive_read_name(
                    &self,
                    request: Request<$sensitive_read_request>,
                ) -> GrpcResult<$sensitive_read_response> {
                    let started = Instant::now();
                    let request_bytes = request.get_ref().encoded_len();
                    let result = match self.authorize(&request, v2::PermissionScope::Control) {
                        Ok(_) => self.application
                            .$sensitive_read_name(request.into_inner())
                            .await
                            .map(Response::new)
                            .map_err(grpc_status),
                        Err(error) => Err(error),
                    };
                    observe_unary(
                        concat!("ddb.api.v2.DebuggerService/", stringify!($sensitive_read_name)),
                        started,
                        request_bytes,
                        &result,
                    );
                    result
                }
            )*
        }

        #[tonic::async_trait]
        impl DebuggerControlService for GrpcApi {
            $(
                async fn $control_sync_name(
                    &self,
                    request: Request<$control_sync_request>,
                ) -> GrpcResult<v2::OperationAdmissionResponse> {
                    let started = Instant::now();
                    let request_bytes = request.get_ref().encoded_len();
                    let result = match self.authorize(&request, v2::PermissionScope::Control) {
                        Ok(principal) => self.application
                            .$control_sync_name(&principal, request.into_inner())
                            .map(Response::new)
                            .map_err(grpc_status),
                        Err(error) => Err(error),
                    };
                    observe_unary(
                        concat!("ddb.api.v2.DebuggerControlService/", stringify!($control_sync_name)),
                        started,
                        request_bytes,
                        &result,
                    );
                    result
                }
            )*
            $(
                async fn $control_async_name(
                    &self,
                    request: Request<$control_async_request>,
                ) -> GrpcResult<v2::OperationAdmissionResponse> {
                    let started = Instant::now();
                    let request_bytes = request.get_ref().encoded_len();
                    let result = match self.authorize(&request, v2::PermissionScope::Control) {
                        Ok(principal) => self.application
                            .$control_async_name(&principal, request.into_inner())
                            .await
                            .map(Response::new)
                            .map_err(grpc_status),
                        Err(error) => Err(error),
                    };
                    observe_unary(
                        concat!("ddb.api.v2.DebuggerControlService/", stringify!($control_async_name)),
                        started,
                        request_bytes,
                        &result,
                    );
                    result
                }
            )*
        }
    };
}

implement_grpc_api! {
    public_sync {
        get_server_info: v2::GetServerInfoRequest => v2::GetServerInfoResponse,
    }
    read_sync {
        get_breakpoint: v2::GetBreakpointRequest => v2::GetBreakpointResponse,
        get_operation: v2::GetOperationRequest => v2::GetOperationResponse,
        list_extension_states: v2::ListExtensionStatesRequest => v2::ListExtensionStatesResponse,
        get_extension_schema: v2::GetExtensionSchemaRequest => v2::GetExtensionSchemaResponse,
    }
    read_async {
        get_capabilities: v2::GetCapabilitiesRequest => v2::GetCapabilitiesResponse,
        get_snapshot: v2::GetSnapshotRequest => v2::GetSnapshotResponse,
        list_sessions: v2::ListSessionsRequest => v2::ListSessionsResponse,
        get_session: v2::GetSessionRequest => v2::GetSessionResponse,
        list_groups: v2::ListGroupsRequest => v2::ListGroupsResponse,
        get_group: v2::GetGroupRequest => v2::GetGroupResponse,
        list_processes: v2::ListProcessesRequest => v2::ListProcessesResponse,
        get_process: v2::GetProcessRequest => v2::GetProcessResponse,
        list_threads: v2::ListThreadsRequest => v2::ListThreadsResponse,
        get_thread: v2::GetThreadRequest => v2::GetThreadResponse,
        get_execution_state: v2::GetExecutionStateRequest => v2::GetExecutionStateResponse,
        list_frames: v2::ListFramesRequest => v2::ListFramesResponse,
        list_scopes: v2::ListScopesRequest => v2::ListScopesResponse,
        list_variables: v2::ListVariablesRequest => v2::ListVariablesResponse,
        expand_variable: v2::ExpandVariableRequest => v2::ExpandVariableResponse,
        list_registers: v2::ListRegistersRequest => v2::ListRegistersResponse,
        list_signals: v2::ListSignalsRequest => v2::ListSignalsResponse,
        resolve_source: v2::ResolveSourceRequest => v2::ResolveSourceResponse,
        read_source: v2::ReadSourceRequest => v2::ReadSourceResponse,
        list_breakpoints: v2::ListBreakpointsRequest => v2::ListBreakpointsResponse,
        list_pending_commands: v2::ListPendingCommandsRequest => v2::ListPendingCommandsResponse,
        list_operations: v2::ListOperationsRequest => v2::ListOperationsResponse,
    }
    sensitive_read_async {
        read_memory: v2::ReadMemoryRequest => v2::ReadMemoryResponse,
    }
    control_sync {
        cancel_operation: v2::CancelOperationRequest,
    }
    control_async {
        execute: v2::ExecuteRequest,
        select_thread: v2::SelectThreadRequest,
        evaluate: v2::EvaluateRequest,
        create_breakpoint: v2::CreateBreakpointRequest,
        update_breakpoint: v2::UpdateBreakpointRequest,
        delete_breakpoint: v2::DeleteBreakpointRequest,
        execute_raw_command: v2::ExecuteRawCommandRequest,
        run_distributed_backtrace: v2::RunDistributedBacktraceRequest,
        invoke_extension_action: v2::InvokeExtensionActionRequest,
    }
}

#[tonic::async_trait]
impl DdbAdminService for GrpcApi {
    async fn get_health(
        &self,
        request: Request<v2::GetHealthRequest>,
    ) -> GrpcResult<v2::GetHealthResponse> {
        let started = Instant::now();
        let request_bytes = request.get_ref().encoded_len();
        attach_grpc_trace_parent(&request);
        let result = self
            .application
            .get_health(request.into_inner())
            .map(Response::new)
            .map_err(grpc_status);
        observe_unary(
            "ddb.api.v2.DdbAdminService/get_health",
            started,
            request_bytes,
            &result,
        );
        result
    }

    async fn get_readiness(
        &self,
        request: Request<v2::GetReadinessRequest>,
    ) -> GrpcResult<v2::GetReadinessResponse> {
        let started = Instant::now();
        let request_bytes = request.get_ref().encoded_len();
        attach_grpc_trace_parent(&request);
        let result = self
            .application
            .get_readiness(request.into_inner())
            .map(Response::new)
            .map_err(grpc_status);
        observe_unary(
            "ddb.api.v2.DdbAdminService/get_readiness",
            started,
            request_bytes,
            &result,
        );
        result
    }

    async fn shutdown(
        &self,
        request: Request<v2::ShutdownRequest>,
    ) -> GrpcResult<v2::OperationAdmissionResponse> {
        let started = Instant::now();
        let request_bytes = request.get_ref().encoded_len();
        let result = match self.authorize(&request, v2::PermissionScope::Admin) {
            Ok(principal) => self
                .application
                .shutdown_request(&principal, request.into_inner())
                .map(Response::new)
                .map_err(grpc_status),
            Err(error) => Err(error),
        };
        observe_unary(
            "ddb.api.v2.DdbAdminService/shutdown",
            started,
            request_bytes,
            &result,
        );
        result
    }
}

#[tonic::async_trait]
impl DdbEventService for GrpcApi {
    type SubscribeStateEventsStream = StateEventStream;

    async fn subscribe_state_events(
        &self,
        request: Request<v2::SubscribeStateEventsRequest>,
    ) -> GrpcResult<Self::SubscribeStateEventsStream> {
        let started = Instant::now();
        let request_bytes = request.get_ref().encoded_len();
        let result: GrpcResult<Self::SubscribeStateEventsStream> =
            match self.authorize(&request, v2::PermissionScope::Read) {
                Ok(_) => match self
                    .application
                    .subscribe_state_events(request.into_inner())
                    .map_err(grpc_status)
                {
                    Ok(subscription) => {
                        let events = stream::unfold(subscription, |mut subscription| async move {
                            subscription
                                .recv()
                                .await
                                .map(|event| (Ok(event), subscription))
                        });
                        Ok(Response::new(Box::pin(events)))
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
        observe_stream(
            "ddb.api.v2.DdbEventService/subscribe_state_events",
            started,
            request_bytes,
            &result,
        );
        result
    }

    type SubscribeOutputStream = OutputEventStream;

    async fn subscribe_output(
        &self,
        request: Request<v2::SubscribeOutputRequest>,
    ) -> GrpcResult<Self::SubscribeOutputStream> {
        let started = Instant::now();
        let request_bytes = request.get_ref().encoded_len();
        let result: GrpcResult<Self::SubscribeOutputStream> =
            match self.authorize(&request, v2::PermissionScope::Read) {
                Ok(_) => match self
                    .application
                    .subscribe_output(request.into_inner())
                    .map_err(grpc_status)
                {
                    Ok(subscription) => {
                        let events = stream::unfold(subscription, |mut subscription| async move {
                            subscription
                                .recv()
                                .await
                                .map(|event| (Ok(event), subscription))
                        });
                        Ok(Response::new(Box::pin(events)))
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
        observe_stream(
            "ddb.api.v2.DdbEventService/subscribe_output",
            started,
            request_bytes,
            &result,
        );
        result
    }
}

/// Feature-gated native listener. The endpoint remains loopback-only and
/// non-default until transport benchmarks and the transport ADR are complete.
pub(crate) struct GrpcPreviewServer {
    addr: SocketAddr,
    application: Arc<DdbApplicationService>,
    authorization: Arc<ApiAuthorization>,
}

impl GrpcPreviewServer {
    pub(crate) fn new(
        addr: SocketAddr,
        application: Arc<DdbApplicationService>,
        authorization: Arc<ApiAuthorization>,
    ) -> Self {
        Self {
            addr,
            application,
            authorization,
        }
    }

    pub(crate) async fn run(
        &self,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let api = GrpcApi::new(
            Arc::clone(&self.application),
            Arc::clone(&self.authorization),
        );
        let debugger = bounded_debugger_server(api.clone());
        let control = bounded_control_server(api.clone());
        let admin = bounded_admin_server(api.clone());
        let events = bounded_event_server(api);

        let (health_reporter, health) = tonic_health::server::health_reporter();
        health_reporter
            .set_serving::<DebuggerServiceServer<GrpcApi>>()
            .await;
        health_reporter
            .set_serving::<DebuggerControlServiceServer<GrpcApi>>()
            .await;
        health_reporter
            .set_serving::<DdbAdminServiceServer<GrpcApi>>()
            .await;
        health_reporter
            .set_serving::<DdbEventServiceServer<GrpcApi>>()
            .await;

        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(ddb_api_types::V2_FILE_DESCRIPTOR_SET)
            .build_v1()?;
        let health =
            InterceptedService::new(health, read_interceptor(Arc::clone(&self.authorization)));
        let reflection = InterceptedService::new(
            reflection,
            read_interceptor(Arc::clone(&self.authorization)),
        );
        let addr = self.addr;
        info!("[gRPC Preview]: Listening on {addr}");

        Server::builder()
            .concurrency_limit_per_connection(MAX_CONCURRENT_REQUESTS_PER_CONNECTION)
            .max_concurrent_streams(MAX_CONCURRENT_STREAMS_PER_CONNECTION)
            .http2_max_header_list_size(MAX_HEADER_BYTES)
            .http2_keepalive_interval(Some(Duration::from_secs(30)))
            .http2_keepalive_timeout(Some(Duration::from_secs(10)))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .add_service(debugger)
            .add_service(control)
            .add_service(admin)
            .add_service(events)
            .add_service(health)
            .add_service(reflection)
            .serve_with_shutdown(addr, async move {
                let _ = shutdown_rx.changed().await;
            })
            .await?;
        Ok(())
    }
}

fn bounded_debugger_server(api: GrpcApi) -> DebuggerServiceServer<GrpcApi> {
    DebuggerServiceServer::new(api)
        .max_decoding_message_size(MAX_REQUEST_BYTES)
        .max_encoding_message_size(MAX_RESPONSE_BYTES)
}

fn bounded_control_server(api: GrpcApi) -> DebuggerControlServiceServer<GrpcApi> {
    DebuggerControlServiceServer::new(api)
        .max_decoding_message_size(MAX_REQUEST_BYTES)
        .max_encoding_message_size(MAX_RESPONSE_BYTES)
}

fn bounded_admin_server(api: GrpcApi) -> DdbAdminServiceServer<GrpcApi> {
    DdbAdminServiceServer::new(api)
        .max_decoding_message_size(MAX_REQUEST_BYTES)
        .max_encoding_message_size(MAX_RESPONSE_BYTES)
}

fn bounded_event_server(api: GrpcApi) -> DdbEventServiceServer<GrpcApi> {
    DdbEventServiceServer::new(api)
        .max_decoding_message_size(MAX_REQUEST_BYTES)
        .max_encoding_message_size(MAX_RESPONSE_BYTES)
}

fn read_interceptor(
    authorization: Arc<ApiAuthorization>,
) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |request| {
        attach_grpc_trace_parent(&request);
        authorization
            .authenticate_authorization(authorization_metadata(&request), v2::PermissionScope::Read)
            .map_err(grpc_status)?;
        Ok(request)
    }
}

fn authorization_metadata<T>(request: &Request<T>) -> Option<&str> {
    request
        .metadata()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
}

fn grpc_status(error: ApplicationError) -> Status {
    let code = match error.code() {
        v2::DdbErrorCode::InvalidArgument => Code::InvalidArgument,
        v2::DdbErrorCode::NotFound => Code::NotFound,
        v2::DdbErrorCode::Conflict
        | v2::DdbErrorCode::FailedPrecondition
        | v2::DdbErrorCode::NotCancellable
        | v2::DdbErrorCode::ReplayGap
        | v2::DdbErrorCode::Expired => Code::FailedPrecondition,
        v2::DdbErrorCode::Unsupported => Code::Unimplemented,
        v2::DdbErrorCode::NotReady | v2::DdbErrorCode::Unavailable => Code::Unavailable,
        v2::DdbErrorCode::Unauthenticated => Code::Unauthenticated,
        v2::DdbErrorCode::PermissionDenied => Code::PermissionDenied,
        v2::DdbErrorCode::ResourceExhausted => Code::ResourceExhausted,
        v2::DdbErrorCode::DeadlineExceeded => Code::DeadlineExceeded,
        v2::DdbErrorCode::Cancelled => Code::Cancelled,
        v2::DdbErrorCode::BackendFailed | v2::DdbErrorCode::PartialFailure => Code::Aborted,
        v2::DdbErrorCode::Internal | v2::DdbErrorCode::Unspecified => Code::Internal,
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let message = error.to_string();
    let details = error.to_contract(request_id).encode_to_vec();
    Status::with_details(code, message, Bytes::from(details))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_status_contains_the_stable_ddb_error_message() {
        let status = grpc_status(ApplicationError::invalid("target", "is required"));
        assert_eq!(status.code(), Code::InvalidArgument);
        let details =
            v2::DdbError::decode(status.details()).expect("status details should be DdbError");
        assert_eq!(details.code, v2::DdbErrorCode::InvalidArgument as i32);
        assert!(!details.request_id.is_empty());
        assert_eq!(details.field_violations.len(), 1);
        assert_eq!(details.field_violations[0].field, "target");
    }

    #[test]
    fn replay_gap_preserves_typed_cursor_details() {
        let earliest = v2::Cursor {
            server_instance_id: "server-a".to_string(),
            sequence: 20,
        };
        let current = v2::Cursor {
            server_instance_id: "server-a".to_string(),
            sequence: 40,
        };
        let status = grpc_status(
            ApplicationError::new(v2::DdbErrorCode::ReplayGap, "rehydration required")
                .with_replay_bounds(earliest.clone(), current.clone()),
        );
        assert_eq!(status.code(), Code::FailedPrecondition);
        let details =
            v2::DdbError::decode(status.details()).expect("status details should be DdbError");
        assert_eq!(details.earliest_cursor, Some(earliest));
        assert_eq!(details.current_cursor, Some(current));
    }
}
