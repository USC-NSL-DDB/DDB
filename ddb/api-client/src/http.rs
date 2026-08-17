use std::{
    collections::HashSet,
    fmt,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bytes::{Bytes, BytesMut};
use ddb_api_types::{v2, wkt::Timestamp};
use futures_util::StreamExt;
use reqwest::{
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
    Client, Response, StatusCode, Url,
};
use serde::{de::DeserializeOwned, Serialize};

use crate::{
    ClientError, NdjsonStream, OutputSync, OutputSyncOptions, ProjectedStateSync, Result,
    StateSync, StateSyncOptions,
};

const DEBUGGER_SERVICE: &str = "ddb.api.v2.DebuggerService";
const CONTROL_SERVICE: &str = "ddb.api.v2.DebuggerControlService";
const EVENT_SERVICE: &str = "ddb.api.v2.DdbEventService";
const ADMIN_SERVICE: &str = "ddb.api.v2.DdbAdminService";

struct PageCollector<T> {
    items: Vec<T>,
    max_items: usize,
    next_token: Option<String>,
    seen_tokens: HashSet<String>,
}

impl<T> PageCollector<T> {
    fn new(first_token: Option<String>, max_items: usize) -> Self {
        let seen_tokens = first_token.iter().cloned().collect();
        Self {
            items: Vec::new(),
            max_items,
            next_token: first_token,
            seen_tokens,
        }
    }

    fn next_token(&self) -> Option<&String> {
        self.next_token.as_ref()
    }

    /// Returns true when the collection is complete.
    fn push_page(&mut self, items: Vec<T>, page: Option<v2::PageInfo>) -> Result<bool> {
        if self.items.len().saturating_add(items.len()) > self.max_items {
            return Err(ClientError::CollectionTooLarge {
                limit: self.max_items,
            });
        }
        self.items.extend(items);
        let Some(next_token) = page.and_then(|page| page.next_page_token) else {
            self.next_token = None;
            return Ok(true);
        };
        if next_token.is_empty() {
            return Err(ClientError::Protocol(
                "server returned an empty pagination token".to_string(),
            ));
        }
        if !self.seen_tokens.insert(next_token.clone()) {
            return Err(ClientError::Protocol(
                "server repeated a pagination token".to_string(),
            ));
        }
        self.next_token = Some(next_token);
        Ok(false)
    }

    fn finish(self) -> Vec<T> {
        self.items
    }
}

/// Runtime bounds and credentials for one reusable HTTP/ProtoJSON client.
#[derive(Clone)]
pub struct ClientConfig {
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_event_bytes: usize,
    /// Number of automatic retries after the first mutation attempt. Every
    /// retry reuses the exact same generated idempotency key and body.
    pub mutation_retries: u32,
    pub mutation_retry_delay: Duration,
}

impl ClientConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            bearer_token: None,
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(30),
            max_request_bytes: 4 * 1024 * 1024,
            max_response_bytes: 16 * 1024 * 1024,
            max_event_bytes: 16 * 1024 * 1024,
            mutation_retries: 2,
            mutation_retry_delay: Duration::from_millis(50),
        }
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("max_event_bytes", &self.max_event_bytes)
            .field("mutation_retries", &self.mutation_retries)
            .field("mutation_retry_delay", &self.mutation_retry_delay)
            .finish()
    }
}

struct Inner {
    http: Client,
    rpc_base: Url,
    bearer_token: Option<String>,
    request_timeout: Duration,
    max_request_bytes: usize,
    max_response_bytes: usize,
    max_event_bytes: usize,
    mutation_retries: u32,
    mutation_retry_delay: Duration,
}

/// Cloneable typed DDB API v2 client.
#[derive(Clone)]
pub struct DdbClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for DdbClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DdbClient")
            .field("rpc_base", &self.inner.rpc_base)
            .field(
                "authenticated",
                &self
                    .inner
                    .bearer_token
                    .as_ref()
                    .map(|_| true)
                    .unwrap_or(false),
            )
            .finish_non_exhaustive()
    }
}

impl DdbClient {
    /// Builds a reusable client. Call `handshake` before relying on a feature.
    pub fn new(config: ClientConfig) -> Result<Self> {
        validate_config(&config)?;
        let mut endpoint = Url::parse(&config.endpoint)
            .map_err(|error| ClientError::InvalidEndpoint(error.to_string()))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(ClientError::InvalidEndpoint(
                "scheme must be http or https".to_string(),
            ));
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(ClientError::InvalidEndpoint(
                "credentials must be supplied with bearer_token, not in the URL".to_string(),
            ));
        }
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        let path = format!("{}/api/v2/rpc/", endpoint.path().trim_end_matches('/'));
        endpoint.set_path(&path);
        let http = Client::builder()
            .connect_timeout(config.connect_timeout)
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                rpc_base: endpoint,
                bearer_token: config.bearer_token,
                request_timeout: config.request_timeout,
                max_request_bytes: config.max_request_bytes,
                max_response_bytes: config.max_response_bytes,
                max_event_bytes: config.max_event_bytes,
                mutation_retries: config.mutation_retries,
                mutation_retry_delay: config.mutation_retry_delay,
            }),
        })
    }

    /// Verifies v2 support and returns the initial discovery documents.
    pub async fn handshake(&self) -> Result<(v2::ServerInfo, v2::Capabilities)> {
        let info = self
            .get_server_info(v2::GetServerInfoRequest::default())
            .await?
            .server_info
            .ok_or_else(|| {
                ClientError::Protocol("GetServerInfo omitted server_info".to_string())
            })?;
        if !info.api_versions.iter().any(|version| version == "v2") {
            return Err(ClientError::Protocol(format!(
                "server {:?} version {:?} advertises API versions {:?}; this client supports API versions [v2]",
                info.name, info.version, info.api_versions
            )));
        }
        let capabilities = self
            .get_capabilities(v2::GetCapabilitiesRequest::default())
            .await?
            .capabilities
            .ok_or_else(|| {
                ClientError::Protocol("GetCapabilities omitted capabilities".to_string())
            })?;
        if capabilities.api_version != "v2" {
            return Err(ClientError::Protocol(format!(
                "server {:?} version {:?} advertises API versions {:?}, but capabilities returned API {:?} with schema {:?}; this client supports API versions [v2]",
                info.name,
                info.version,
                info.api_versions,
                capabilities.api_version,
                capabilities.schema_version
            )));
        }
        Ok((info, capabilities))
    }

    /// Creates a reconnecting snapshot-plus-state-event workflow.
    pub fn state_sync(&self, options: StateSyncOptions) -> Result<StateSync> {
        StateSync::new(self.clone(), options)
    }

    /// Creates a reconnecting state workflow with SDK-owned projection convergence.
    pub fn projected_state_sync(&self, options: StateSyncOptions) -> Result<ProjectedStateSync> {
        ProjectedStateSync::new(self.clone(), options)
    }

    /// Creates an independently reconnecting output workflow.
    pub fn output_sync(&self, options: OutputSyncOptions) -> Result<OutputSync> {
        OutputSync::new(self.clone(), options)
    }

    /// Generates a fresh mutation idempotency key.
    pub fn new_idempotency_key() -> String {
        format!("client_{}", uuid::Uuid::new_v4().simple())
    }

    /// Polls a retained operation until it reaches a terminal state.
    pub async fn wait_operation(
        &self,
        operation_id: impl Into<String>,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<v2::Operation> {
        let operation_id = operation_id.into();
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ClientError::OperationTimeout { operation_id });
            }
            let response = tokio::time::timeout(
                remaining,
                self.get_operation(v2::GetOperationRequest {
                    context: None,
                    operation_id: operation_id.clone(),
                }),
            )
            .await
            .map_err(|_| ClientError::OperationTimeout {
                operation_id: operation_id.clone(),
            })??;
            let operation = response.operation.ok_or_else(|| {
                ClientError::Protocol("GetOperation omitted operation".to_string())
            })?;
            if matches!(
                v2::OperationState::try_from(operation.state),
                Ok(v2::OperationState::Completed
                    | v2::OperationState::Failed
                    | v2::OperationState::Cancelled)
            ) {
                return Ok(operation);
            }
            if Instant::now() >= deadline {
                return Err(ClientError::OperationTimeout { operation_id });
            }
            tokio::time::sleep(
                poll_interval.min(deadline.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }

    /// Collects every session page up to an explicit client-side item bound.
    pub async fn list_all_sessions(&self, max_items: usize) -> Result<Vec<v2::Session>> {
        self.collect_sessions(v2::ListSessionsRequest::default(), max_items)
            .await
    }

    pub async fn subscribe_state_events(
        &self,
        mut request: v2::SubscribeStateEventsRequest,
    ) -> Result<NdjsonStream<v2::StateEvent>> {
        self.prepare_context(&mut request.context, false);
        self.post_stream(EVENT_SERVICE, "SubscribeStateEvents", &request)
            .await
    }

    pub async fn subscribe_output(
        &self,
        mut request: v2::SubscribeOutputRequest,
    ) -> Result<NdjsonStream<v2::OutputEvent>> {
        self.prepare_context(&mut request.context, false);
        self.post_stream(EVENT_SERVICE, "SubscribeOutput", &request)
            .await
    }

    async fn unary<Req, Res>(
        &self,
        service: &str,
        method: &str,
        request: &Req,
        authenticated: bool,
        retry_mutation: bool,
    ) -> Result<Res>
    where
        Req: Serialize + ?Sized,
        Res: DeserializeOwned,
    {
        let body = serde_json::to_vec(request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        if body.len() > self.inner.max_request_bytes {
            return Err(ClientError::PayloadTooLarge {
                limit: self.inner.max_request_bytes,
            });
        }
        let url = self.rpc_url(service, method)?;
        let mut retries_remaining = if retry_mutation {
            self.inner.mutation_retries
        } else {
            0
        };
        let mut retry_number = 0_u32;
        loop {
            let mut builder = self
                .inner
                .http
                .post(url.clone())
                .timeout(self.inner.request_timeout)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .body(body.clone());
            if authenticated {
                builder = self.authorize(builder);
            }
            let result = match builder.send().await {
                Ok(response) => {
                    let status = response.status();
                    match read_bounded(response, self.inner.max_response_bytes).await {
                        Ok(body) if status.is_success() => serde_json::from_slice(&body)
                            .map_err(|error| ClientError::Protocol(error.to_string())),
                        Ok(body) => Err(decode_api_error(status, &body)),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(ClientError::Transport(error)),
            };
            match result {
                Ok(response) => return Ok(response),
                Err(error) if retries_remaining > 0 && error.is_retryable() => {
                    retries_remaining -= 1;
                    retry_number = retry_number.saturating_add(1);
                    tokio::time::sleep(retry_delay(
                        self.inner.mutation_retry_delay,
                        retry_number,
                        &error,
                    ))
                    .await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn post_stream<Req, Event>(
        &self,
        service: &str,
        method: &str,
        request: &Req,
    ) -> Result<NdjsonStream<Event>>
    where
        Req: Serialize + ?Sized,
        Event: DeserializeOwned,
    {
        let body = serde_json::to_vec(request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        if body.len() > self.inner.max_request_bytes {
            return Err(ClientError::PayloadTooLarge {
                limit: self.inner.max_request_bytes,
            });
        }
        let response = self
            .authorize(
                self.inner
                    .http
                    .post(self.rpc_url(service, method)?)
                    .header(CONTENT_TYPE, "application/json")
                    .header(ACCEPT, "application/x-ndjson")
                    .body(body),
            )
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded(response, self.inner.max_response_bytes).await?;
            return Err(decode_api_error(status, &body));
        }
        Ok(NdjsonStream::from_response(
            response,
            self.inner.max_event_bytes,
        ))
    }

    fn rpc_url(&self, service: &str, method: &str) -> Result<Url> {
        self.inner
            .rpc_base
            .join(&format!("{service}/{method}"))
            .map_err(|error| ClientError::InvalidEndpoint(error.to_string()))
    }

    fn authorize(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.inner.bearer_token.as_deref() {
            Some(token) => builder.header(AUTHORIZATION, format!("Bearer {token}")),
            None => builder,
        }
    }

    fn prepare_context(&self, context: &mut Option<v2::RequestContext>, mutation: bool) {
        let context = context.get_or_insert_with(v2::RequestContext::default);
        if context.deadline.is_none() {
            context.deadline = system_deadline(self.inner.request_timeout);
        }
        if mutation && context.idempotency_key.as_deref().is_none_or(str::is_empty) {
            context.idempotency_key = Some(Self::new_idempotency_key());
        }
    }
}

macro_rules! unary_method {
    ($name:ident, $request:ty, $response:ty, $service:expr, $method:literal, $authenticated:expr) => {
        pub async fn $name(&self, mut request: $request) -> Result<$response> {
            self.prepare_context(&mut request.context, false);
            self.unary($service, $method, &request, $authenticated, false)
                .await
        }
    };
}

macro_rules! mutation_method {
    ($name:ident, $request:ty, $service:expr, $method:literal) => {
        pub async fn $name(&self, mut request: $request) -> Result<v2::OperationAdmissionResponse> {
            self.prepare_context(&mut request.context, true);
            self.unary($service, $method, &request, true, true).await
        }
    };
}

macro_rules! collect_pages_method {
    ($name:ident, $single:ident, $request:ty, $response:ty, $item:ty, $items:ident) => {
        pub async fn $name(&self, mut request: $request, max_items: usize) -> Result<Vec<$item>> {
            let page_size = request.page.as_ref().map_or(0, |page| page.page_size);
            let first_token = request
                .page
                .as_ref()
                .and_then(|page| page.page_token.clone());
            let mut collector = PageCollector::new(first_token, max_items);
            loop {
                request.page = Some(v2::PageRequest {
                    page_size,
                    page_token: collector.next_token().cloned(),
                });
                let response: $response = self.$single(request.clone()).await?;
                if collector.push_page(response.$items, response.page)? {
                    return Ok(collector.finish());
                }
            }
        }
    };
}

impl DdbClient {
    unary_method!(
        get_server_info,
        v2::GetServerInfoRequest,
        v2::GetServerInfoResponse,
        DEBUGGER_SERVICE,
        "GetServerInfo",
        false
    );

    collect_pages_method!(
        collect_sessions,
        list_sessions,
        v2::ListSessionsRequest,
        v2::ListSessionsResponse,
        v2::Session,
        sessions
    );
    collect_pages_method!(
        collect_groups,
        list_groups,
        v2::ListGroupsRequest,
        v2::ListGroupsResponse,
        v2::Group,
        groups
    );
    collect_pages_method!(
        collect_processes,
        list_processes,
        v2::ListProcessesRequest,
        v2::ListProcessesResponse,
        v2::Process,
        processes
    );
    collect_pages_method!(
        collect_threads,
        list_threads,
        v2::ListThreadsRequest,
        v2::ListThreadsResponse,
        v2::Thread,
        threads
    );
    collect_pages_method!(
        collect_frames,
        list_frames,
        v2::ListFramesRequest,
        v2::ListFramesResponse,
        v2::Frame,
        frames
    );
    collect_pages_method!(
        collect_scopes,
        list_scopes,
        v2::ListScopesRequest,
        v2::ListScopesResponse,
        v2::Scope,
        scopes
    );
    collect_pages_method!(
        collect_variables,
        list_variables,
        v2::ListVariablesRequest,
        v2::ListVariablesResponse,
        v2::Variable,
        variables
    );
    collect_pages_method!(
        collect_variable_children,
        expand_variable,
        v2::ExpandVariableRequest,
        v2::ExpandVariableResponse,
        v2::Variable,
        variables
    );
    collect_pages_method!(
        collect_registers,
        list_registers,
        v2::ListRegistersRequest,
        v2::ListRegistersResponse,
        v2::Register,
        registers
    );
    collect_pages_method!(
        collect_signals,
        list_signals,
        v2::ListSignalsRequest,
        v2::ListSignalsResponse,
        v2::DebuggerSignal,
        signals
    );
    collect_pages_method!(
        collect_breakpoints,
        list_breakpoints,
        v2::ListBreakpointsRequest,
        v2::ListBreakpointsResponse,
        v2::Breakpoint,
        breakpoints
    );
    collect_pages_method!(
        collect_pending_commands,
        list_pending_commands,
        v2::ListPendingCommandsRequest,
        v2::ListPendingCommandsResponse,
        v2::PendingCommand,
        pending_commands
    );
    collect_pages_method!(
        collect_operations,
        list_operations,
        v2::ListOperationsRequest,
        v2::ListOperationsResponse,
        v2::Operation,
        operations
    );
    collect_pages_method!(
        collect_extension_states,
        list_extension_states,
        v2::ListExtensionStatesRequest,
        v2::ListExtensionStatesResponse,
        v2::ExtensionState,
        extension_states
    );
    unary_method!(
        get_capabilities,
        v2::GetCapabilitiesRequest,
        v2::GetCapabilitiesResponse,
        DEBUGGER_SERVICE,
        "GetCapabilities",
        true
    );
    unary_method!(
        get_snapshot,
        v2::GetSnapshotRequest,
        v2::GetSnapshotResponse,
        DEBUGGER_SERVICE,
        "GetSnapshot",
        true
    );
    unary_method!(
        list_sessions,
        v2::ListSessionsRequest,
        v2::ListSessionsResponse,
        DEBUGGER_SERVICE,
        "ListSessions",
        true
    );
    unary_method!(
        get_session,
        v2::GetSessionRequest,
        v2::GetSessionResponse,
        DEBUGGER_SERVICE,
        "GetSession",
        true
    );
    unary_method!(
        list_groups,
        v2::ListGroupsRequest,
        v2::ListGroupsResponse,
        DEBUGGER_SERVICE,
        "ListGroups",
        true
    );
    unary_method!(
        get_group,
        v2::GetGroupRequest,
        v2::GetGroupResponse,
        DEBUGGER_SERVICE,
        "GetGroup",
        true
    );
    unary_method!(
        list_processes,
        v2::ListProcessesRequest,
        v2::ListProcessesResponse,
        DEBUGGER_SERVICE,
        "ListProcesses",
        true
    );
    unary_method!(
        get_process,
        v2::GetProcessRequest,
        v2::GetProcessResponse,
        DEBUGGER_SERVICE,
        "GetProcess",
        true
    );
    unary_method!(
        list_threads,
        v2::ListThreadsRequest,
        v2::ListThreadsResponse,
        DEBUGGER_SERVICE,
        "ListThreads",
        true
    );
    unary_method!(
        get_thread,
        v2::GetThreadRequest,
        v2::GetThreadResponse,
        DEBUGGER_SERVICE,
        "GetThread",
        true
    );
    unary_method!(
        get_execution_state,
        v2::GetExecutionStateRequest,
        v2::GetExecutionStateResponse,
        DEBUGGER_SERVICE,
        "GetExecutionState",
        true
    );
    unary_method!(
        list_frames,
        v2::ListFramesRequest,
        v2::ListFramesResponse,
        DEBUGGER_SERVICE,
        "ListFrames",
        true
    );
    unary_method!(
        list_scopes,
        v2::ListScopesRequest,
        v2::ListScopesResponse,
        DEBUGGER_SERVICE,
        "ListScopes",
        true
    );
    unary_method!(
        list_variables,
        v2::ListVariablesRequest,
        v2::ListVariablesResponse,
        DEBUGGER_SERVICE,
        "ListVariables",
        true
    );
    unary_method!(
        expand_variable,
        v2::ExpandVariableRequest,
        v2::ExpandVariableResponse,
        DEBUGGER_SERVICE,
        "ExpandVariable",
        true
    );
    unary_method!(
        list_signals,
        v2::ListSignalsRequest,
        v2::ListSignalsResponse,
        DEBUGGER_SERVICE,
        "ListSignals",
        true
    );
    unary_method!(
        list_registers,
        v2::ListRegistersRequest,
        v2::ListRegistersResponse,
        DEBUGGER_SERVICE,
        "ListRegisters",
        true
    );
    unary_method!(
        read_memory,
        v2::ReadMemoryRequest,
        v2::ReadMemoryResponse,
        DEBUGGER_SERVICE,
        "ReadMemory",
        true
    );
    unary_method!(
        resolve_source,
        v2::ResolveSourceRequest,
        v2::ResolveSourceResponse,
        DEBUGGER_SERVICE,
        "ResolveSource",
        true
    );
    unary_method!(
        read_source,
        v2::ReadSourceRequest,
        v2::ReadSourceResponse,
        DEBUGGER_SERVICE,
        "ReadSource",
        true
    );
    unary_method!(
        list_breakpoints,
        v2::ListBreakpointsRequest,
        v2::ListBreakpointsResponse,
        DEBUGGER_SERVICE,
        "ListBreakpoints",
        true
    );
    unary_method!(
        get_breakpoint,
        v2::GetBreakpointRequest,
        v2::GetBreakpointResponse,
        DEBUGGER_SERVICE,
        "GetBreakpoint",
        true
    );
    unary_method!(
        list_pending_commands,
        v2::ListPendingCommandsRequest,
        v2::ListPendingCommandsResponse,
        DEBUGGER_SERVICE,
        "ListPendingCommands",
        true
    );
    unary_method!(
        get_operation,
        v2::GetOperationRequest,
        v2::GetOperationResponse,
        DEBUGGER_SERVICE,
        "GetOperation",
        true
    );
    unary_method!(
        list_operations,
        v2::ListOperationsRequest,
        v2::ListOperationsResponse,
        DEBUGGER_SERVICE,
        "ListOperations",
        true
    );
    unary_method!(
        list_extension_states,
        v2::ListExtensionStatesRequest,
        v2::ListExtensionStatesResponse,
        DEBUGGER_SERVICE,
        "ListExtensionStates",
        true
    );
    unary_method!(
        get_extension_schema,
        v2::GetExtensionSchemaRequest,
        v2::GetExtensionSchemaResponse,
        DEBUGGER_SERVICE,
        "GetExtensionSchema",
        true
    );
    unary_method!(
        get_health,
        v2::GetHealthRequest,
        v2::GetHealthResponse,
        ADMIN_SERVICE,
        "GetHealth",
        false
    );
    unary_method!(
        get_readiness,
        v2::GetReadinessRequest,
        v2::GetReadinessResponse,
        ADMIN_SERVICE,
        "GetReadiness",
        false
    );

    mutation_method!(execute, v2::ExecuteRequest, CONTROL_SERVICE, "Execute");
    mutation_method!(
        select_thread,
        v2::SelectThreadRequest,
        CONTROL_SERVICE,
        "SelectThread"
    );
    mutation_method!(evaluate, v2::EvaluateRequest, CONTROL_SERVICE, "Evaluate");
    mutation_method!(
        create_breakpoint,
        v2::CreateBreakpointRequest,
        CONTROL_SERVICE,
        "CreateBreakpoint"
    );
    mutation_method!(
        update_breakpoint,
        v2::UpdateBreakpointRequest,
        CONTROL_SERVICE,
        "UpdateBreakpoint"
    );
    mutation_method!(
        delete_breakpoint,
        v2::DeleteBreakpointRequest,
        CONTROL_SERVICE,
        "DeleteBreakpoint"
    );
    mutation_method!(
        execute_raw_command,
        v2::ExecuteRawCommandRequest,
        CONTROL_SERVICE,
        "ExecuteRawCommand"
    );
    mutation_method!(
        run_distributed_backtrace,
        v2::RunDistributedBacktraceRequest,
        CONTROL_SERVICE,
        "RunDistributedBacktrace"
    );
    mutation_method!(
        invoke_extension_action,
        v2::InvokeExtensionActionRequest,
        CONTROL_SERVICE,
        "InvokeExtensionAction"
    );
    mutation_method!(
        cancel_operation,
        v2::CancelOperationRequest,
        CONTROL_SERVICE,
        "CancelOperation"
    );
    mutation_method!(shutdown, v2::ShutdownRequest, ADMIN_SERVICE, "Shutdown");
}

async fn read_bounded(response: Response, limit: usize) -> Result<Bytes> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > limit as u64)
    {
        return Err(ClientError::PayloadTooLarge { limit });
    }
    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ClientError::PayloadTooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

fn decode_api_error(status: StatusCode, body: &[u8]) -> ClientError {
    match serde_json::from_slice::<v2::DdbError>(body) {
        // ProtoJSON deliberately ignores unknown fields and fills omitted
        // scalars with defaults. Validate the mandatory semantic envelope so
        // a legacy or proxy JSON body such as {"api_version":"v1"} cannot be
        // mistaken for a typed v2 error.
        Ok(error)
            if error.code != v2::DdbErrorCode::Unspecified as i32
                && !error.message.trim().is_empty() =>
        {
            ClientError::Api {
                status,
                message: error.message.clone(),
                error: Box::new(error),
            }
        }
        Ok(_) | Err(_) => ClientError::Http {
            status,
            message: String::from_utf8_lossy(body).chars().take(512).collect(),
        },
    }
}

fn validate_config(config: &ClientConfig) -> Result<()> {
    if config.connect_timeout.is_zero() {
        return Err(ClientError::InvalidConfig(
            "connect_timeout must be greater than zero".to_string(),
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(ClientError::InvalidConfig(
            "request_timeout must be greater than zero".to_string(),
        ));
    }
    if [
        ("max_request_bytes", config.max_request_bytes),
        ("max_response_bytes", config.max_response_bytes),
        ("max_event_bytes", config.max_event_bytes),
    ]
    .into_iter()
    .any(|(_, value)| value == 0)
    {
        return Err(ClientError::InvalidConfig(
            "payload limits must be greater than zero".to_string(),
        ));
    }
    if config.mutation_retries > 0 && config.mutation_retry_delay.is_zero() {
        return Err(ClientError::InvalidConfig(
            "mutation_retry_delay must be greater than zero when retries are enabled".to_string(),
        ));
    }
    Ok(())
}

fn retry_delay(base: Duration, retry_number: u32, error: &ClientError) -> Duration {
    let exponent = retry_number.saturating_sub(1).min(16);
    let calculated = base.saturating_mul(1_u32 << exponent);
    error
        .ddb_error()
        .and_then(|error| error.retry_after.as_ref())
        .and_then(proto_duration)
        .map_or(calculated, |suggested| calculated.max(suggested))
}

pub(crate) fn proto_duration(duration: &ddb_api_types::wkt::Duration) -> Option<Duration> {
    if duration.seconds < 0 || duration.nanos < 0 || duration.nanos >= 1_000_000_000 {
        return None;
    }
    Some(Duration::new(
        u64::try_from(duration.seconds).ok()?,
        u32::try_from(duration.nanos).ok()?,
    ))
}

fn system_deadline(timeout: Duration) -> Option<Timestamp> {
    let deadline = SystemTime::now().checked_add(timeout)?;
    let since_epoch = deadline.duration_since(UNIX_EPOCH).ok()?;
    Some(Timestamp {
        seconds: i64::try_from(since_epoch.as_secs()).ok()?,
        nanos: since_epoch.subsec_nanos() as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_prefix_is_preserved_and_credentials_are_redacted() {
        let config =
            ClientConfig::new("https://debug.example/prefix").with_bearer_token("secret-token");
        assert!(!format!("{config:?}").contains("secret-token"));
        let client = DdbClient::new(config).unwrap();
        assert_eq!(
            client
                .rpc_url(DEBUGGER_SERVICE, "GetServerInfo")
                .unwrap()
                .as_str(),
            "https://debug.example/prefix/api/v2/rpc/ddb.api.v2.DebuggerService/GetServerInfo"
        );
        assert!(!format!("{client:?}").contains("secret-token"));
    }

    #[test]
    fn invalid_endpoint_schemes_fail_before_network_access() {
        assert!(matches!(
            DdbClient::new(ClientConfig::new("file:///tmp/ddb")),
            Err(ClientError::InvalidEndpoint(_))
        ));
    }

    #[test]
    fn page_collector_enforces_bounds_and_rejects_repeated_or_empty_tokens() {
        let mut bounded = PageCollector::new(None, 1);
        assert!(matches!(
            bounded.push_page(vec![1, 2], None),
            Err(ClientError::CollectionTooLarge { limit: 1 })
        ));

        let mut repeated = PageCollector::<u8>::new(Some("cursor-a".to_string()), 4);
        assert!(matches!(
            repeated.push_page(
                Vec::new(),
                Some(v2::PageInfo {
                    next_page_token: Some("cursor-a".to_string()),
                })
            ),
            Err(ClientError::Protocol(_))
        ));

        let mut empty = PageCollector::<u8>::new(None, 4);
        assert!(matches!(
            empty.push_page(
                Vec::new(),
                Some(v2::PageInfo {
                    next_page_token: Some(String::new()),
                })
            ),
            Err(ClientError::Protocol(_))
        ));
    }

    #[test]
    fn only_an_explicit_missing_v2_route_allows_migration_fallback() {
        let missing = decode_api_error(StatusCode::NOT_FOUND, br#"{"api_version":"v1"}"#);
        assert!(missing.is_api_version_unavailable());

        let empty_protojson = decode_api_error(StatusCode::NOT_FOUND, b"{}");
        assert!(empty_protojson.is_api_version_unavailable());

        let typed_not_found = decode_api_error(
            StatusCode::NOT_FOUND,
            &serde_json::to_vec(&v2::DdbError {
                code: v2::DdbErrorCode::NotFound as i32,
                message: "thread not found".to_string(),
                ..Default::default()
            })
            .unwrap(),
        );
        assert!(!typed_not_found.is_api_version_unavailable());

        let unauthenticated = decode_api_error(
            StatusCode::UNAUTHORIZED,
            &serde_json::to_vec(&v2::DdbError {
                code: v2::DdbErrorCode::Unauthenticated as i32,
                message: "token required".to_string(),
                ..Default::default()
            })
            .unwrap(),
        );
        assert!(!unauthenticated.is_api_version_unavailable());

        let malformed = decode_api_error(StatusCode::OK, b"not-json");
        assert!(!malformed.is_api_version_unavailable());
    }
}
