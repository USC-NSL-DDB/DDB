use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ddb_api_types::v2::{
    output_event, ApiLimits, Capabilities, ComponentHealth, Cursor, DdbErrorCode, DebuggerSignal,
    ExecutionAction, ExpandVariableRequest, ExpandVariableResponse, ExtensionSchemaDocument,
    GetBreakpointRequest, GetBreakpointResponse, GetCapabilitiesRequest, GetCapabilitiesResponse,
    GetExecutionStateRequest, GetExecutionStateResponse, GetExtensionSchemaRequest,
    GetExtensionSchemaResponse, GetGroupRequest, GetGroupResponse, GetHealthRequest,
    GetHealthResponse, GetOperationRequest, GetOperationResponse, GetProcessRequest,
    GetProcessResponse, GetReadinessRequest, GetReadinessResponse, GetServerInfoRequest,
    GetServerInfoResponse, GetSessionRequest, GetSessionResponse, GetSnapshotRequest,
    GetSnapshotResponse, GetThreadRequest, GetThreadResponse, HealthReport, HealthStatus,
    ListBreakpointsRequest, ListBreakpointsResponse, ListExtensionStatesRequest,
    ListExtensionStatesResponse, ListFramesRequest, ListFramesResponse, ListGroupsRequest,
    ListGroupsResponse, ListOperationsRequest, ListOperationsResponse, ListPendingCommandsRequest,
    ListPendingCommandsResponse, ListProcessesRequest, ListProcessesResponse, ListRegistersRequest,
    ListRegistersResponse, ListScopesRequest, ListScopesResponse, ListSessionsRequest,
    ListSessionsResponse, ListSignalsRequest, ListSignalsResponse, ListThreadsRequest,
    ListThreadsResponse, ListVariablesRequest, ListVariablesResponse, MemoryBlock, OperationKind,
    OperationState, OutputEvent, OutputGap, OutputStreamKind, ReadMemoryRequest,
    ReadMemoryResponse, ReadSourceRequest, ReadSourceResponse, Register, RegisterFormat,
    ResolveSourceRequest, ResolveSourceResponse, ResourceKind, Selection, ServerInfo, Snapshot,
    SnapshotSection, SourceContent, SourceFile, StateEvent, StateEventKind, SubscribeOutputRequest,
    SubscribeStateEventsRequest, TransportEndpoint, TransportKind, WireEncoding,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    api::{
        read_model::ApiQueries,
        telemetry::{
            record_output_gap, record_output_truncation, record_replay_gap, record_subscriber_delta,
        },
    },
    cmd_flow::{
        output_hub::{
            DebuggerOutputStream, OutputDelivery, OutputHub, OutputHubError, OutputSubscription,
        },
        router::Target as CommandTarget,
    },
    common::{config::ApiResourceLimits, Config},
    shutdown::ShutdownCtrl,
    status::RuntimeStatus,
};

use super::debugger_reads::{
    decode_empty_done, decode_frames, decode_memory, decode_register_names, decode_register_values,
    decode_signals, decode_variable_children, decode_variable_object_name, decode_variables,
    DecodedVariable,
};
use super::{
    collection_revision, ApplicationCommandPort, ApplicationError, OpaqueIdRegistry,
    OperationStore, OperationStoreConfig, PageCodec, ProjectionContext, RequestScope,
    ResolvedTarget, ResourceCatalog, ResourceIdKind, StateJournal, StateJournalConfig,
    StateSubscription, TargetPurpose, TargetResolver,
};

const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 200;
const MAX_EXTENSION_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_REGISTER_NAMES: usize = 4_096;
const MAX_SIGNAL_COUNT: usize = 4_096;
const MAX_ADDRESS_BYTES: usize = 1_024;
const MAX_VARIABLE_IDENTITY_BYTES: usize = 64 * 1024;
const MAX_VARIABLE_DEPTH: usize = 32;
const VARIABLE_OBJECT_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug)]
pub(super) struct StopFrameKey {
    pub(super) internal: String,
    pub(super) global_thread_id: u64,
    pub(super) execution_revision: u64,
    pub(super) level: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoppedTargetSnapshot(Vec<(u64, u64)>);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VariableIdentity {
    version: u8,
    frame_key: String,
    root_ordinal: u32,
    expression: String,
    path: Vec<u32>,
}

pub(crate) struct ApplicationStateSubscription {
    inner: StateSubscription,
    journal: StateJournal,
    request_id: String,
    kinds: HashSet<i32>,
    resource_kinds: HashSet<i32>,
    session_ids: HashSet<String>,
    group_ids: HashSet<String>,
    include_extensions: bool,
    terminated: bool,
}

impl ApplicationStateSubscription {
    pub(crate) async fn recv(&mut self) -> Option<StateEvent> {
        if self.terminated {
            return None;
        }
        loop {
            match self.inner.recv().await {
                Ok(Some(envelope)) => {
                    let mut event = envelope.event;
                    if event.kind == StateEventKind::RequiredResync as i32 {
                        return Some(event);
                    }
                    if !self.kinds.is_empty() && !self.kinds.contains(&event.kind) {
                        continue;
                    }
                    if !self.resource_kinds.is_empty()
                        && !self.resource_kinds.contains(&event.resource_kind)
                    {
                        continue;
                    }
                    if !envelope.context.global
                        && ((!self.session_ids.is_empty() || !self.group_ids.is_empty())
                            && !envelope
                                .context
                                .session_ids
                                .iter()
                                .any(|id| self.session_ids.contains(id))
                            && !envelope
                                .context
                                .group_ids
                                .iter()
                                .any(|id| self.group_ids.contains(id)))
                    {
                        continue;
                    }
                    if !self.include_extensions {
                        if event.resource_kind == ResourceKind::ExtensionState as i32 {
                            continue;
                        }
                        event.extension_details.clear();
                    }
                    return Some(event);
                }
                Ok(None) => {
                    self.terminated = true;
                    return None;
                }
                Err(error) => {
                    self.terminated = true;
                    let (cursor, state_revision) = self.journal.checkpoint();
                    record_replay_gap("state");
                    return Some(StateEvent {
                        cursor: Some(cursor),
                        state_revision,
                        schema_version: "2.0".to_string(),
                        occurred_at: Some(super::timestamp_now()),
                        request_id: Some(self.request_id.clone()),
                        operation_id: None,
                        kind: StateEventKind::RequiredResync as i32,
                        resource_kind: ResourceKind::Unspecified as i32,
                        resource_id: String::new(),
                        resource_revision: 0,
                        payload: Some(ddb_api_types::v2::state_event::Payload::RequiredResync(
                            ddb_api_types::v2::RequiredResync {
                                reason: Some(error.to_contract(&self.request_id)),
                            },
                        )),
                        extension_details: Vec::new(),
                    });
                }
            }
        }
    }
}

pub(crate) struct ApplicationOutputSubscription {
    inner: OutputSubscription,
    ids: Arc<OpaqueIdRegistry>,
    server_instance_id: String,
    streams: HashSet<i32>,
    session_ids: HashSet<u64>,
}

impl ApplicationOutputSubscription {
    pub(crate) async fn recv(&mut self) -> Option<OutputEvent> {
        loop {
            match self.inner.recv().await? {
                OutputDelivery::Gap(gap) => {
                    record_output_gap(gap.dropped_events);
                    return Some(OutputEvent {
                        cursor: Some(Cursor {
                            server_instance_id: self.server_instance_id.clone(),
                            sequence: gap.last_missing_sequence,
                        }),
                        occurred_at: Some(super::timestamp_now()),
                        session_id: None,
                        thread_id: None,
                        stream: OutputStreamKind::Unspecified as i32,
                        content: None,
                        gap: Some(OutputGap {
                            first_missing_sequence: gap.first_missing_sequence,
                            last_missing_sequence: gap.last_missing_sequence,
                            dropped_events: Some(gap.dropped_events),
                            dropped_bytes: gap.dropped_bytes,
                            reason: gap.reason.to_string(),
                        }),
                        truncated: false,
                    });
                }
                OutputDelivery::Record(record) => {
                    let record = Arc::unwrap_or_clone(record);
                    if record.truncated {
                        record_output_truncation();
                    }
                    let stream = public_output_stream(record.stream);
                    if !self.streams.is_empty() && !self.streams.contains(&(stream as i32)) {
                        continue;
                    }
                    if !self.session_ids.is_empty()
                        && !record
                            .session_id
                            .is_some_and(|sid| self.session_ids.contains(&sid))
                    {
                        continue;
                    }
                    let session_id = match record.session_id {
                        Some(sid) => match self.ids.encode(ResourceIdKind::Session, sid) {
                            Ok(id) => Some(id),
                            Err(_) => {
                                return Some(output_projection_gap(
                                    &self.server_instance_id,
                                    record.sequence,
                                    record.observed_at,
                                ));
                            }
                        },
                        None => None,
                    };
                    return Some(OutputEvent {
                        cursor: Some(Cursor {
                            server_instance_id: self.server_instance_id.clone(),
                            sequence: record.sequence,
                        }),
                        occurred_at: Some(super::context::system_time_to_timestamp(
                            record.observed_at,
                        )),
                        session_id,
                        thread_id: None,
                        stream: stream as i32,
                        content: Some(output_event::Content::Text(record.text)),
                        gap: None,
                        truncated: record.truncated,
                    });
                }
            }
        }
    }
}

impl Drop for ApplicationOutputSubscription {
    fn drop(&mut self) {
        record_subscriber_delta("output", -1);
    }
}

fn public_output_stream(stream: DebuggerOutputStream) -> OutputStreamKind {
    match stream {
        DebuggerOutputStream::Console => OutputStreamKind::Console,
        DebuggerOutputStream::Log => OutputStreamKind::Log,
        DebuggerOutputStream::Target => OutputStreamKind::Target,
        DebuggerOutputStream::InferiorStdout => OutputStreamKind::InferiorStdout,
        DebuggerOutputStream::InferiorStderr => OutputStreamKind::InferiorStderr,
        DebuggerOutputStream::Prompt => OutputStreamKind::Prompt,
    }
}

fn output_projection_gap(
    server_instance_id: &str,
    sequence: u64,
    observed_at: SystemTime,
) -> OutputEvent {
    OutputEvent {
        cursor: Some(Cursor {
            server_instance_id: server_instance_id.to_string(),
            sequence,
        }),
        occurred_at: Some(super::context::system_time_to_timestamp(observed_at)),
        session_id: None,
        thread_id: None,
        stream: OutputStreamKind::Unspecified as i32,
        content: None,
        gap: Some(OutputGap {
            first_missing_sequence: sequence,
            last_missing_sequence: sequence,
            dropped_events: Some(1),
            dropped_bytes: None,
            reason: "output context could not be projected".to_string(),
        }),
        truncated: false,
    }
}

#[derive(Clone)]
pub(crate) struct DdbApplicationConfig {
    transports: Vec<TransportEndpoint>,
    pub(super) limits: ApiLimits,
    authentication_mode: String,
}

impl DdbApplicationConfig {
    pub(crate) fn http(
        endpoint_uri: impl Into<String>,
        tls_required: bool,
        authentication_mode: impl Into<String>,
        resource_limits: &ApiResourceLimits,
    ) -> Self {
        let limits = ApiLimits {
            max_page_size: MAX_PAGE_SIZE as u32,
            preferred_page_size: DEFAULT_PAGE_SIZE as u32,
            max_request_bytes: 4 * 1024 * 1024,
            max_response_bytes: 16 * 1024 * 1024,
            max_memory_read_bytes: 1024 * 1024,
            max_source_lines: 2_000,
            max_variable_children: 500,
            max_state_replay_events: resource_limits.state_replay_events as u64,
            max_state_replay_bytes: resource_limits.state_replay_bytes as u64,
            state_replay_retention_millis: resource_limits.state_replay_retention_millis,
            state_subscriber_queue: resource_limits.state_subscriber_queue as u32,
            output_subscriber_queue: resource_limits.output_subscriber_queue as u32,
            max_subscribers: resource_limits.max_subscribers as u32,
            max_operation_records: resource_limits.operation_records as u32,
            operation_retention_millis: resource_limits.operation_retention_millis,
            max_dynamic_value_depth: 32,
            max_extension_payload_bytes: MAX_EXTENSION_PAYLOAD_BYTES as u64,
            max_operation_bytes: resource_limits.operation_bytes as u64,
            max_operation_record_bytes: resource_limits.operation_record_bytes as u64,
            max_output_event_bytes: resource_limits.output_event_bytes as u64,
            max_source_bytes: MAX_SOURCE_BYTES,
        };
        Self {
            transports: vec![TransportEndpoint {
                transport: TransportKind::Http as i32,
                uri: endpoint_uri.into(),
                encodings: vec![WireEncoding::Protojson as i32, WireEncoding::Json as i32],
                tls_required,
            }],
            limits,
            authentication_mode: authentication_mode.into(),
        }
    }

    #[cfg(feature = "grpc-preview")]
    pub(crate) fn with_grpc_preview(
        mut self,
        endpoint_uri: impl Into<String>,
        tls_required: bool,
    ) -> Self {
        self.transports.push(TransportEndpoint {
            transport: TransportKind::Grpc as i32,
            uri: endpoint_uri.into(),
            encodings: vec![WireEncoding::Protobuf as i32],
            tls_required,
        });
        self
    }
}

/// One semantic implementation shared by HTTP, native, and future adapters.
pub(crate) struct DdbApplicationService {
    pub(super) queries: Arc<ApiQueries>,
    pub(super) command_port: Arc<dyn ApplicationCommandPort>,
    pub(super) config: Arc<Config>,
    runtime_status: Arc<RuntimeStatus>,
    pub(super) shutdown_ctrl: Arc<ShutdownCtrl>,
    server_instance_id: String,
    started_at: ddb_api_types::wkt::Timestamp,
    pub(super) api_config: DdbApplicationConfig,
    pub(super) ids: Arc<OpaqueIdRegistry>,
    pub(super) resources: ResourceCatalog,
    pages: PageCodec,
    pub(super) operations: OperationStore,
    pub(super) journal: StateJournal,
    output: Arc<OutputHub>,
    runtime_event_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl DdbApplicationService {
    pub(crate) fn new(
        queries: Arc<ApiQueries>,
        command_port: Arc<dyn ApplicationCommandPort>,
        output: Arc<OutputHub>,
        config: Arc<Config>,
        runtime_status: Arc<RuntimeStatus>,
        shutdown_ctrl: Arc<ShutdownCtrl>,
        api_config: DdbApplicationConfig,
    ) -> Arc<Self> {
        let runtime_changes = queries.subscribe_runtime_changes();
        let pending_changes = queries.subscribe_pending_changes();
        let server_instance_id = format!("ddb_{}", uuid::Uuid::new_v4().simple());
        let operation_config = OperationStoreConfig {
            max_records: api_config.limits.max_operation_records as usize,
            max_bytes: api_config.limits.max_operation_bytes as usize,
            max_record_bytes: api_config.limits.max_operation_record_bytes as usize,
            retention: std::time::Duration::from_millis(
                api_config.limits.operation_retention_millis,
            ),
            max_idempotency_key_bytes: 256,
        };
        let journal_config = StateJournalConfig {
            max_events: api_config.limits.max_state_replay_events as usize,
            max_bytes: api_config.limits.max_state_replay_bytes as usize,
            retention: std::time::Duration::from_millis(
                api_config.limits.state_replay_retention_millis,
            ),
            subscriber_queue: api_config.limits.state_subscriber_queue as usize,
            max_subscribers: api_config.limits.max_subscribers as usize,
        };
        let service = Arc::new(Self {
            queries,
            command_port,
            config,
            runtime_status,
            shutdown_ctrl,
            started_at: super::context::system_time_to_timestamp(SystemTime::now()),
            api_config,
            ids: Arc::new(OpaqueIdRegistry::new(100_000)),
            resources: ResourceCatalog::new(100_000, 256, 8 * 1024 * 1024),
            pages: PageCodec::new(DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE),
            operations: OperationStore::new(&server_instance_id, operation_config),
            journal: StateJournal::new(&server_instance_id, journal_config),
            output,
            runtime_event_tasks: Mutex::new(Vec::new()),
            server_instance_id,
        });
        let tasks = vec![
            service.spawn_runtime_event_bridge(runtime_changes),
            service.spawn_pending_event_bridge(pending_changes),
        ];
        *service
            .runtime_event_tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = tasks;
        service
    }

    pub(crate) fn validate_response_bytes(
        &self,
        encoded_bytes: usize,
    ) -> Result<(), ApplicationError> {
        if encoded_bytes as u64 > self.api_config.limits.max_response_bytes {
            return Err(ApplicationError::resource_exhausted(format!(
                "encoded response exceeds the {} byte response limit",
                self.api_config.limits.max_response_bytes
            )));
        }
        Ok(())
    }

    pub(crate) fn server_instance_id(&self) -> &str {
        &self.server_instance_id
    }

    pub(crate) fn subscribe_state_events(
        &self,
        request: SubscribeStateEventsRequest,
    ) -> Result<ApplicationStateSubscription, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let filter = request.filter.unwrap_or_default();
        let session_ids = filter
            .session_ids
            .iter()
            .map(|id| {
                self.ids.decode(ResourceIdKind::Session, id)?;
                Ok(id.clone())
            })
            .collect::<Result<HashSet<_>, ApplicationError>>()?;
        let group_ids = filter
            .group_ids
            .iter()
            .map(|id| {
                self.ids.decode(ResourceIdKind::Group, id)?;
                Ok(id.clone())
            })
            .collect::<Result<HashSet<_>, ApplicationError>>()?;
        let mut kinds = HashSet::new();
        for value in filter.kinds {
            let kind = StateEventKind::try_from(value).map_err(|_| {
                ApplicationError::invalid("filter.kinds", "contains an unknown event kind")
            })?;
            if kind == StateEventKind::Unspecified {
                return Err(ApplicationError::invalid(
                    "filter.kinds",
                    "must not contain UNSPECIFIED",
                ));
            }
            kinds.insert(value);
        }
        let mut resource_kinds = HashSet::new();
        for value in filter.resource_kinds {
            let kind = ResourceKind::try_from(value).map_err(|_| {
                ApplicationError::invalid(
                    "filter.resource_kinds",
                    "contains an unknown resource kind",
                )
            })?;
            if kind == ResourceKind::Unspecified {
                return Err(ApplicationError::invalid(
                    "filter.resource_kinds",
                    "must not contain UNSPECIFIED",
                ));
            }
            resource_kinds.insert(value);
        }
        let inner = self.journal.subscribe(request.after_cursor.as_ref())?;
        Ok(ApplicationStateSubscription {
            inner,
            journal: self.journal.clone(),
            request_id: scope.request_id().to_string(),
            kinds,
            resource_kinds,
            session_ids,
            group_ids,
            include_extensions: filter.include_extensions,
            terminated: false,
        })
    }

    pub(crate) fn subscribe_output(
        &self,
        request: SubscribeOutputRequest,
    ) -> Result<ApplicationOutputSubscription, ApplicationError> {
        let _scope = RequestScope::begin(request.context.as_ref())?;
        let filter = request.filter.unwrap_or_default();
        if !filter.thread_ids.is_empty() {
            return Err(ApplicationError::new(
                DdbErrorCode::Unsupported,
                "thread-filtered output is unavailable because debugger streams have session scope",
            )
            .requiring("output.thread_context"));
        }
        let mut streams = HashSet::new();
        for value in filter.streams {
            let stream = OutputStreamKind::try_from(value).map_err(|_| {
                ApplicationError::invalid("filter.streams", "contains an unknown output stream")
            })?;
            if stream == OutputStreamKind::Unspecified {
                return Err(ApplicationError::invalid(
                    "filter.streams",
                    "must not contain UNSPECIFIED",
                ));
            }
            streams.insert(value);
        }
        let session_ids = filter
            .session_ids
            .iter()
            .map(|id| {
                self.ids
                    .decode(ResourceIdKind::Session, id)?
                    .parse::<u64>()
                    .map_err(|_| ApplicationError::not_found("session"))
            })
            .collect::<Result<HashSet<_>, _>>()?;
        let after_sequence = request
            .after_cursor
            .as_ref()
            .map(|cursor| {
                if cursor.server_instance_id != self.server_instance_id {
                    record_replay_gap("output");
                    return Err(ApplicationError::new(
                        DdbErrorCode::ReplayGap,
                        "output cursor belongs to another server instance",
                    ));
                }
                let current = self.output.current_sequence();
                if cursor.sequence > current {
                    return Err(ApplicationError::invalid(
                        "after_cursor.sequence",
                        "is ahead of the current output cursor",
                    ));
                }
                Ok(cursor.sequence)
            })
            .transpose()?;
        let inner = self
            .output
            .subscribe(after_sequence)
            .map_err(output_hub_error)?;
        record_subscriber_delta("output", 1);
        Ok(ApplicationOutputSubscription {
            inner,
            ids: Arc::clone(&self.ids),
            server_instance_id: self.server_instance_id.clone(),
            streams,
            session_ids,
        })
    }

    pub(crate) fn shutdown(&self) {
        for task in self
            .runtime_event_tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain(..)
        {
            task.abort();
        }
        self.journal.shutdown();
        self.output.shutdown();
    }

    pub(crate) fn get_server_info(
        &self,
        request: GetServerInfoRequest,
    ) -> Result<GetServerInfoResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        Ok(GetServerInfoResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            server_info: Some(ServerInfo {
                name: "ddb".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                build_revision: option_env!("DDB_BUILD_REVISION").map(str::to_string),
                server_instance_id: self.server_instance_id.clone(),
                started_at: Some(self.started_at),
                api_versions: vec!["v1".to_string(), "v2".to_string()],
            }),
        })
    }

    pub(crate) async fn get_capabilities(
        &self,
        request: GetCapabilitiesRequest,
    ) -> Result<GetCapabilitiesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        if let Some(target) = request.target.as_ref() {
            // DDB currently configures one debugger backend per server, so the
            // effective feature set is identical for every target. Resolving
            // here still rejects stale/invalid scopes rather than pretending
            // that an unknown target has capabilities.
            TargetResolver::new(self.queries.as_ref(), &self.ids)
                .resolve(Some(target), TargetPurpose::Breakpoint)
                .await?;
            scope.ensure_active()?;
        }
        Ok(GetCapabilitiesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            capabilities: Some(self.capabilities()?),
        })
    }

    pub(crate) async fn list_sessions(
        &self,
        request: ListSessionsRequest,
    ) -> Result<ListSessionsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let projection = self.projection();
        let sessions = self
            .queries
            .sessions()
            .await
            .iter()
            .map(|session| projection.session(session))
            .collect::<Result<Vec<_>, _>>()?;
        let revision = collection_revision(&sessions);
        let page = self
            .pages
            .paginate("sessions", revision, sessions, request.page.as_ref())?;
        Ok(ListSessionsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            sessions: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn get_session(
        &self,
        request: GetSessionRequest,
    ) -> Result<GetSessionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let internal_id = self
            .ids
            .decode(ResourceIdKind::Session, &request.session_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("session"))?;
        let view = self
            .queries
            .sessions()
            .await
            .into_iter()
            .find(|session| session.sid == internal_id)
            .ok_or_else(|| ApplicationError::not_found("session"))?;
        Ok(GetSessionResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            session: Some(self.projection().session(&view)?),
        })
    }

    pub(crate) async fn list_processes(
        &self,
        request: ListProcessesRequest,
    ) -> Result<ListProcessesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let (session_ids, exact_process_id, collection) = match request.target.as_ref() {
            Some(target) => {
                let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(Some(target), TargetPurpose::Command)
                    .await?;
                let exact_process_id = match &resolved.command {
                    CommandTarget::Thread(thread_id) => Some(
                        self.queries
                            .thread_by_id(thread_id.value())
                            .await
                            .ok_or_else(|| ApplicationError::not_found("thread"))?
                            .process_id,
                    ),
                    _ => None,
                };
                let target_key = format!("{:x}", Sha256::digest(target.encode_to_vec()));
                (resolved.session_ids, exact_process_id, target_key)
            }
            None => {
                let session_ids = self
                    .queries
                    .sessions()
                    .await
                    .into_iter()
                    .map(|session| session.sid)
                    .collect();
                (session_ids, None, "all".to_string())
            }
        };
        scope.ensure_active()?;

        let mut views = self.queries.processes_for_sessions(&session_ids).await;
        if let Some(exact_process_id) = exact_process_id {
            views.retain(|process| Some(process.global_id) == exact_process_id);
        }
        scope.ensure_active()?;
        let projection = self.projection();
        let processes = views
            .iter()
            .map(|process| projection.process(process))
            .collect::<Result<Vec<_>, _>>()?;
        let revision = collection_revision(&processes);
        let page = self.pages.paginate(
            &format!("processes:{collection}"),
            revision,
            processes,
            request.page.as_ref(),
        )?;
        Ok(ListProcessesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            processes: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn get_process(
        &self,
        request: GetProcessRequest,
    ) -> Result<GetProcessResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let internal_id = self
            .ids
            .decode(ResourceIdKind::Process, &request.process_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("process"))?;
        let view = self
            .queries
            .process_by_id(internal_id)
            .await
            .ok_or_else(|| ApplicationError::not_found("process"))?;
        scope.ensure_active()?;
        Ok(GetProcessResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            process: Some(self.projection().process(&view)?),
        })
    }

    pub(crate) async fn list_threads(
        &self,
        request: ListThreadsRequest,
    ) -> Result<ListThreadsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        let exact_thread_id = match resolved.command {
            CommandTarget::Thread(thread_id) => Some(thread_id.value()),
            _ => None,
        };
        let projection = self.projection();
        let threads = self
            .queries
            .threads_for_sessions(&resolved.session_ids)
            .await
            .into_iter()
            .filter(|thread| exact_thread_id.is_none_or(|id| thread.global_id == id))
            .map(|thread| projection.thread(&thread))
            .collect::<Result<Vec<_>, _>>()?;
        let revision = collection_revision(&threads);
        let collection = format!(
            "threads:{}:{}",
            resolved
                .session_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
            exact_thread_id.map_or_else(|| "*".to_string(), |id| id.to_string())
        );
        let page = self
            .pages
            .paginate(&collection, revision, threads, request.page.as_ref())?;
        Ok(ListThreadsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            threads: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn get_thread(
        &self,
        request: GetThreadRequest,
    ) -> Result<GetThreadResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let internal_id = self
            .ids
            .decode(ResourceIdKind::Thread, &request.thread_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("thread"))?;
        let view = self
            .queries
            .thread_by_id(internal_id)
            .await
            .ok_or_else(|| ApplicationError::not_found("thread"))?;
        Ok(GetThreadResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            thread: Some(self.projection().thread(&view)?),
        })
    }

    pub(crate) async fn list_frames(
        &self,
        request: ListFramesRequest,
    ) -> Result<ListFramesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let global_thread_id = self
            .ids
            .decode(ResourceIdKind::Thread, &request.thread_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("thread"))?;
        let before = self
            .queries
            .thread_by_id(global_thread_id)
            .await
            .ok_or_else(|| ApplicationError::not_found("thread"))?;
        if before.status != "stopped" {
            return Err(ApplicationError::new(
                DdbErrorCode::FailedPrecondition,
                "stack frames are available only while the thread is stopped",
            ));
        }

        let collection = format!("frames:{global_thread_id}");
        let window = self.pages.window(
            &collection,
            before.execution_revision,
            request.page.as_ref(),
        )?;
        let low = u32::try_from(window.offset).map_err(|_| {
            ApplicationError::new(
                DdbErrorCode::Expired,
                "frame page offset is no longer valid",
            )
        })?;
        let high = u32::try_from(window.offset.saturating_add(window.size)).map_err(|_| {
            ApplicationError::new(DdbErrorCode::Expired, "frame page range is no longer valid")
        })?;
        let outcome = scope
            .wait(self.command_port.execute(
                &format!("-stack-list-frames {low} {high}"),
                CommandTarget::Thread(crate::state::GlobalThreadId::new(global_thread_id)),
            ))
            .await?
            .map_err(|_| ApplicationError::backend("debugger stack query failed"))?;
        let after = self
            .queries
            .thread_by_id(global_thread_id)
            .await
            .ok_or_else(|| ApplicationError::not_found("thread"))?;
        if after.status != "stopped" || after.execution_revision != before.execution_revision {
            return Err(ApplicationError::new(
                DdbErrorCode::Expired,
                "thread execution changed while stack frames were being read",
            )
            .retryable(true));
        }

        let mut decoded = decode_frames(&outcome)?;
        decoded.sort_unstable_by_key(|frame| frame.level);
        if decoded.len() > window.size.saturating_add(1)
            || decoded
                .iter()
                .any(|frame| frame.level < low || frame.level > high)
            || decoded
                .windows(2)
                .any(|pair| pair[0].level == pair[1].level)
        {
            return Err(ApplicationError::backend(
                "debugger returned an invalid bounded stack range",
            ));
        }
        let projection = self.projection();
        let frames = decoded
            .iter()
            .map(|frame| projection.frame(frame, global_thread_id, before.execution_revision))
            .collect::<Result<Vec<_>, _>>()?;
        let page = self
            .pages
            .finish_window(&collection, before.execution_revision, window, frames);
        Ok(ListFramesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            frames: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn get_execution_state(
        &self,
        request: GetExecutionStateRequest,
    ) -> Result<GetExecutionStateResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        let exact_thread_id = match resolved.command {
            CommandTarget::Thread(thread_id) => Some(thread_id.value()),
            _ => None,
        };
        let threads = self
            .queries
            .threads_for_sessions(&resolved.session_ids)
            .await
            .into_iter()
            .filter(|thread| exact_thread_id.is_none_or(|id| thread.global_id == id))
            .collect::<Vec<_>>();
        if threads.is_empty() || threads.iter().all(|thread| thread.status == "unavailable") {
            return Err(ApplicationError::new(
                DdbErrorCode::NotReady,
                "target execution state is not available yet",
            )
            .retryable(true));
        }
        let (_, execution_state) = self
            .projection()
            .execution_state(resolved.public, &threads)?
            .ok_or_else(|| {
                ApplicationError::new(
                    DdbErrorCode::NotReady,
                    "target execution state is not available yet",
                )
                .retryable(true)
            })?;
        Ok(GetExecutionStateResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            execution_state: Some(execution_state),
        })
    }

    pub(crate) async fn list_scopes(
        &self,
        request: ListScopesRequest,
    ) -> Result<ListScopesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let frame = self.current_frame(&request.frame_id).await?;
        let scopes = vec![self.projection().locals_scope(&frame.internal)?];
        let page = self.pages.paginate(
            &format!("scopes:{}", frame.internal),
            frame.execution_revision,
            scopes,
            request.page.as_ref(),
        )?;
        Ok(ListScopesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            scopes: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn list_variables(
        &self,
        request: ListVariablesRequest,
    ) -> Result<ListVariablesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let scope_key = self.ids.decode(ResourceIdKind::Scope, &request.scope_id)?;
        let frame_key = scope_key
            .strip_suffix(":locals")
            .ok_or_else(|| ApplicationError::not_found("scope"))?;
        let frame = self.current_frame_key(frame_key).await?;
        let outcome = scope
            .wait(self.command_port.execute(
                &format!(
                    "-stack-list-variables --thread {} --frame {} --all-values",
                    frame.global_thread_id, frame.level
                ),
                CommandTarget::Thread(crate::state::GlobalThreadId::new(frame.global_thread_id)),
            ))
            .await?
            .map_err(|_| ApplicationError::backend("debugger variable query failed"))?;
        self.ensure_frame_current(&frame).await?;
        let decoded = decode_variables(&outcome)?;
        if decoded.len() > self.api_config.limits.max_variable_children as usize {
            return Err(ApplicationError::resource_exhausted(
                "debugger variable collection exceeds the advertised bound",
            ));
        }
        let projection = self.projection();
        let variables = decoded
            .iter()
            .enumerate()
            .map(|(index, variable)| {
                let identity = VariableIdentity {
                    version: 1,
                    frame_key: frame.internal.clone(),
                    root_ordinal: u32::try_from(index).map_err(|_| {
                        ApplicationError::backend("debugger variable index overflowed")
                    })?,
                    expression: variable.name.clone(),
                    path: Vec::new(),
                };
                let internal_id = encode_variable_identity(&identity)?;
                projection.variable(variable, &internal_id, Some(variable.name.clone()), None)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let page = self.pages.paginate(
            &format!("variables:{scope_key}"),
            frame.execution_revision,
            variables,
            request.page.as_ref(),
        )?;
        Ok(ListVariablesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            variables: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn expand_variable(
        &self,
        request: ExpandVariableRequest,
    ) -> Result<ExpandVariableResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let identity = self.decode_variable_identity(&request.variable_id)?;
        if identity.path.len() >= MAX_VARIABLE_DEPTH {
            return Err(ApplicationError::resource_exhausted(
                "variable expansion exceeds the advertised depth bound",
            ));
        }
        let frame = self.current_frame_key(&identity.frame_key).await?;
        let collection = format!("variable-children:{}", request.variable_id);
        let window =
            self.pages
                .window(&collection, frame.execution_revision, request.page.as_ref())?;
        let target =
            CommandTarget::Thread(crate::state::GlobalThreadId::new(frame.global_thread_id));
        let requested_object_name = format!("ddb_api_{}", uuid::Uuid::new_v4().simple());
        let expression = serde_json::to_string(&identity.expression)
            .expect("serializing a validated expression cannot fail");
        let create_command = format!(
            "-var-create --thread {} --frame {} {requested_object_name} * {expression}",
            frame.global_thread_id, frame.level
        );

        scope.ensure_active()?;
        let create_outcome = match scope
            .wait(self.command_port.execute(&create_command, target.clone()))
            .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => {
                self.schedule_variable_object_cleanup(&requested_object_name, target);
                return Err(ApplicationError::backend(
                    "debugger variable-object creation failed",
                ));
            }
            Err(error) => {
                self.schedule_variable_object_cleanup(&requested_object_name, target);
                return Err(error);
            }
        };
        scope.ensure_active()?;
        self.ensure_frame_current(&frame).await?;
        let object_name = decode_variable_object_name(&create_outcome)?;
        validate_variable_object_name(&object_name)?;

        let expansion = self
            .read_variable_children(
                &scope,
                &frame,
                &identity,
                &object_name,
                window.offset,
                window.size,
            )
            .await;

        let delete_command = format!(
            "-var-delete {}",
            serde_json::to_string(&object_name)
                .expect("serializing a validated variable-object name cannot fail")
        );
        let cleanup = match scope
            .wait(self.command_port.execute(&delete_command, target.clone()))
            .await
        {
            Ok(result) => result
                .map_err(|_| ApplicationError::backend("debugger variable-object cleanup failed"))
                .and_then(|outcome| decode_empty_done(&outcome, "variable-object cleanup")),
            Err(error) => {
                self.schedule_variable_object_cleanup(&object_name, target);
                Err(error)
            }
        };

        let (variables, backend_has_more) = expansion?;
        cleanup?;
        scope.ensure_active()?;
        self.ensure_frame_current(&frame).await?;
        let page = self.pages.finish_window_with_more(
            &collection,
            frame.execution_revision,
            window,
            variables,
            backend_has_more,
        );
        Ok(ExpandVariableResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            variables: page.items,
            page: Some(page.info),
        })
    }

    fn schedule_variable_object_cleanup(&self, object_name: &str, target: CommandTarget) {
        let command_port = Arc::clone(&self.command_port);
        let delete_command = format!(
            "-var-delete {}",
            serde_json::to_string(object_name)
                .expect("serializing a validated variable-object name cannot fail")
        );
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                VARIABLE_OBJECT_CLEANUP_TIMEOUT,
                command_port.execute(&delete_command, target),
            )
            .await;
        });
    }

    #[allow(clippy::too_many_arguments)]
    async fn read_variable_children(
        &self,
        scope: &RequestScope,
        frame: &StopFrameKey,
        identity: &VariableIdentity,
        root_object_name: &str,
        offset: usize,
        page_size: usize,
    ) -> Result<(Vec<ddb_api_types::v2::Variable>, bool), ApplicationError> {
        let target =
            CommandTarget::Thread(crate::state::GlobalThreadId::new(frame.global_thread_id));
        let mut object_name = root_object_name.to_string();
        for index in &identity.path {
            let high = index.checked_add(1).ok_or_else(|| {
                ApplicationError::new(
                    DdbErrorCode::Expired,
                    "variable child identity no longer addresses the collection",
                )
            })?;
            let command = variable_children_command(&object_name, *index, high);
            let outcome = scope
                .wait(self.command_port.execute(&command, target.clone()))
                .await?
                .map_err(|_| ApplicationError::backend("debugger variable traversal failed"))?;
            scope.ensure_active()?;
            self.ensure_frame_current(frame).await?;
            let mut children = decode_variable_children(&outcome)?.children;
            if children.len() != 1 {
                return Err(ApplicationError::new(
                    DdbErrorCode::Expired,
                    "variable child identity no longer addresses the collection",
                ));
            }
            object_name = children.remove(0).object_name;
            validate_variable_object_name(&object_name)?;
        }

        let low = u32::try_from(offset).map_err(|_| {
            ApplicationError::new(
                DdbErrorCode::Expired,
                "variable page offset no longer addresses the collection",
            )
        })?;
        let high = offset
            .checked_add(page_size)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ApplicationError::new(
                    DdbErrorCode::Expired,
                    "variable page range no longer addresses the collection",
                )
            })?;
        let command = variable_children_command(&object_name, low, high);
        let outcome = scope
            .wait(self.command_port.execute(&command, target))
            .await?
            .map_err(|_| ApplicationError::backend("debugger variable-child query failed"))?;
        scope.ensure_active()?;
        self.ensure_frame_current(frame).await?;
        let decoded = decode_variable_children(&outcome)?;
        if decoded.children.len() > page_size.saturating_add(1) {
            return Err(ApplicationError::backend(
                "debugger returned an invalid bounded variable-child range",
            ));
        }
        let projection = self.projection();
        let variables = decoded
            .children
            .into_iter()
            .enumerate()
            .map(|(position, child)| {
                let child_index = offset
                    .checked_add(position)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| {
                        ApplicationError::backend("debugger variable child index overflowed")
                    })?;
                let mut child_identity = identity.clone();
                child_identity.path.push(child_index);
                let internal_id = encode_variable_identity(&child_identity)?;
                let variable = DecodedVariable {
                    name: child.display_name,
                    value: child.value,
                    type_name: child.type_name,
                    child_count: child.child_count,
                };
                projection.variable(&variable, &internal_id, None, child.presentation_hint)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((variables, decoded.has_more))
    }

    pub(crate) async fn list_registers(
        &self,
        request: ListRegistersRequest,
    ) -> Result<ListRegistersResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let frame = self.current_frame(&request.frame_id).await?;
        let format = RegisterFormat::try_from(request.format).map_err(|_| {
            ApplicationError::invalid("format", "contains an unknown register format")
        })?;
        let format = match format {
            RegisterFormat::Unspecified => RegisterFormat::Natural,
            value => value,
        };
        let collection = format!("registers:{}:{}", frame.internal, format as i32);
        let window =
            self.pages
                .window(&collection, frame.execution_revision, request.page.as_ref())?;
        let target =
            CommandTarget::Thread(crate::state::GlobalThreadId::new(frame.global_thread_id));

        scope.ensure_active()?;
        let names_outcome = scope
            .wait(
                self.command_port
                    .execute("-data-list-register-names", target.clone()),
            )
            .await?
            .map_err(|_| ApplicationError::backend("debugger register-name query failed"))?;
        scope.ensure_active()?;
        self.ensure_frame_current(&frame).await?;
        let names = decode_register_names(&names_outcome)?;
        if names.len() > MAX_REGISTER_NAMES {
            return Err(ApplicationError::resource_exhausted(
                "debugger register-name collection exceeds the server bound",
            ));
        }
        let named = names
            .into_iter()
            .enumerate()
            .filter(|(_, name)| !name.is_empty())
            .map(|(number, name)| {
                u32::try_from(number)
                    .map(|number| (number, name))
                    .map_err(|_| ApplicationError::backend("debugger register index overflowed"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if window.offset > named.len() {
            return Err(ApplicationError::new(
                DdbErrorCode::Expired,
                "register page token no longer addresses this collection",
            ));
        }
        let selected = named
            .into_iter()
            .skip(window.offset)
            .take(window.size.saturating_add(1))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            let page = self.pages.finish_window(
                &collection,
                frame.execution_revision,
                window,
                Vec::<Register>::new(),
            );
            return Ok(ListRegistersResponse {
                context: Some(scope.response_context(&self.server_instance_id)),
                registers: page.items,
                page: Some(page.info),
            });
        }

        let numbers = selected
            .iter()
            .map(|(number, _)| number.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let natural_command = format!(
            "-data-list-register-values --thread {} --frame {} N {numbers}",
            frame.global_thread_id, frame.level
        );
        let natural_outcome = scope
            .wait(self.command_port.execute(&natural_command, target.clone()))
            .await?
            .map_err(|_| ApplicationError::backend("debugger register-value query failed"))?;
        scope.ensure_active()?;
        self.ensure_frame_current(&frame).await?;
        let natural_values = decode_register_values(&natural_outcome)?;
        validate_register_numbers(&selected, &natural_values)?;

        let formatted_values = if format == RegisterFormat::Natural {
            None
        } else {
            let format_code = match format {
                RegisterFormat::Hexadecimal => 'x',
                RegisterFormat::Decimal => 'd',
                RegisterFormat::Binary => 't',
                RegisterFormat::Natural | RegisterFormat::Unspecified => unreachable!(),
            };
            let command = format!(
                "-data-list-register-values --thread {} --frame {} {format_code} {numbers}",
                frame.global_thread_id, frame.level
            );
            let outcome = scope
                .wait(self.command_port.execute(&command, target))
                .await?
                .map_err(|_| {
                    ApplicationError::backend("debugger formatted-register query failed")
                })?;
            scope.ensure_active()?;
            self.ensure_frame_current(&frame).await?;
            let values = decode_register_values(&outcome)?;
            validate_register_numbers(&selected, &values)?;
            Some(values)
        };

        let registers = selected
            .into_iter()
            .map(|(number, name)| {
                let natural = natural_values.get(&number).cloned().unwrap_or_default();
                let unavailable = register_unavailable(&natural);
                Register {
                    name,
                    value: natural,
                    formatted_value: (!unavailable)
                        .then(|| {
                            formatted_values
                                .as_ref()
                                .and_then(|values| values.get(&number))
                                .cloned()
                        })
                        .flatten(),
                    unavailable,
                }
            })
            .collect::<Vec<_>>();
        let page =
            self.pages
                .finish_window(&collection, frame.execution_revision, window, registers);
        Ok(ListRegistersResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            registers: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn list_signals(
        &self,
        request: ListSignalsRequest,
    ) -> Result<ListSignalsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        if resolved.session_ids.len() != 1 {
            return Err(ApplicationError::invalid(
                "target",
                "signal catalogs require a target that resolves to exactly one session",
            ));
        }
        let session_id = resolved.session_ids[0];
        scope.ensure_active()?;
        let outcome = scope
            .wait(
                self.command_port
                    .execute("-list-signals", CommandTarget::Session(session_id)),
            )
            .await?
            .map_err(|_| ApplicationError::backend("debugger signal query failed"))?;
        scope.ensure_active()?;
        let decoded = decode_signals(&outcome)?;
        if decoded.len() > MAX_SIGNAL_COUNT {
            return Err(ApplicationError::resource_exhausted(
                "debugger signal catalog exceeds the server bound",
            ));
        }
        let signals = decoded
            .into_iter()
            .map(|signal| DebuggerSignal {
                name: signal.name,
                stop: signal.stop,
                print: signal.print,
                pass: signal.pass,
                description: signal.description.filter(|value| !value.is_empty()),
            })
            .collect::<Vec<_>>();
        let revision = collection_revision(&signals);
        let page = self.pages.paginate(
            &format!("signals:{session_id}"),
            revision,
            signals,
            request.page.as_ref(),
        )?;
        Ok(ListSignalsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            signals: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn read_memory(
        &self,
        request: ReadMemoryRequest,
    ) -> Result<ReadMemoryResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let address = request.address.trim();
        if address.is_empty() {
            return Err(ApplicationError::invalid("address", "must not be empty"));
        }
        if address.len() > MAX_ADDRESS_BYTES {
            return Err(ApplicationError::invalid(
                "address",
                format!("must not exceed {MAX_ADDRESS_BYTES} bytes"),
            ));
        }
        if request.byte_count == 0 {
            return Err(ApplicationError::invalid(
                "byte_count",
                "must be greater than zero",
            ));
        }
        if request.byte_count > self.api_config.limits.max_memory_read_bytes {
            return Err(ApplicationError::invalid(
                "byte_count",
                format!(
                    "must not exceed {} bytes",
                    self.api_config.limits.max_memory_read_bytes
                ),
            ));
        }
        let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        let before = self.stopped_target_snapshot(&resolved).await?;
        let quoted_address =
            serde_json::to_string(address).expect("serializing a validated string cannot fail");
        let command = format!(
            "-data-read-memory-bytes {quoted_address} {}",
            request.byte_count
        );
        scope.ensure_active()?;
        let outcome = scope
            .wait(
                self.command_port
                    .execute(&command, resolved.command.clone()),
            )
            .await?
            .map_err(|_| ApplicationError::backend("debugger memory read failed"))?;
        scope.ensure_active()?;
        self.ensure_stopped_target_current(&resolved, &before)
            .await?;
        let decoded = decode_memory(&outcome, address, request.byte_count)?;
        Ok(ReadMemoryResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            memory: Some(MemoryBlock {
                address: decoded.address,
                data: decoded.data,
                unreadable_bytes: decoded.unreadable_bytes,
            }),
        })
    }

    pub(super) async fn current_frame(
        &self,
        public_frame_id: &str,
    ) -> Result<StopFrameKey, ApplicationError> {
        let frame_key = self.ids.decode(ResourceIdKind::Frame, public_frame_id)?;
        self.current_frame_key(&frame_key).await
    }

    async fn current_frame_key(&self, frame_key: &str) -> Result<StopFrameKey, ApplicationError> {
        let mut parts = frame_key.split(':');
        let (Some(thread), Some(revision), Some(level), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ApplicationError::not_found("frame"));
        };
        let frame = StopFrameKey {
            internal: frame_key.to_string(),
            global_thread_id: thread
                .parse::<u64>()
                .map_err(|_| ApplicationError::not_found("frame"))?,
            execution_revision: revision
                .parse::<u64>()
                .map_err(|_| ApplicationError::not_found("frame"))?,
            level: level
                .parse::<u32>()
                .map_err(|_| ApplicationError::not_found("frame"))?,
        };
        self.ensure_frame_current(&frame).await?;
        Ok(frame)
    }

    pub(super) async fn ensure_frame_current(
        &self,
        frame: &StopFrameKey,
    ) -> Result<(), ApplicationError> {
        let thread = self
            .queries
            .thread_by_id(frame.global_thread_id)
            .await
            .ok_or_else(|| ApplicationError::not_found("frame thread"))?;
        if thread.status != "stopped" {
            return Err(ApplicationError::new(
                DdbErrorCode::FailedPrecondition,
                "frame data is available only while the thread is stopped",
            ));
        }
        if thread.execution_revision != frame.execution_revision {
            return Err(ApplicationError::new(
                DdbErrorCode::Expired,
                "frame identity expired because thread execution changed",
            ));
        }
        Ok(())
    }

    fn decode_variable_identity(
        &self,
        public_variable_id: &str,
    ) -> Result<VariableIdentity, ApplicationError> {
        let encoded = self
            .ids
            .decode(ResourceIdKind::Variable, public_variable_id)?;
        if encoded.len() > MAX_VARIABLE_IDENTITY_BYTES {
            return Err(ApplicationError::not_found("variable"));
        }
        let identity = serde_json::from_str::<VariableIdentity>(&encoded)
            .map_err(|_| ApplicationError::not_found("variable"))?;
        if identity.version != 1
            || identity.frame_key.is_empty()
            || identity.expression.trim().is_empty()
            || identity.expression.len() > MAX_VARIABLE_IDENTITY_BYTES
            || identity.path.len() > MAX_VARIABLE_DEPTH
        {
            return Err(ApplicationError::not_found("variable"));
        }
        Ok(identity)
    }

    async fn stopped_target_snapshot(
        &self,
        resolved: &ResolvedTarget,
    ) -> Result<StoppedTargetSnapshot, ApplicationError> {
        if resolved.resolved_target_count != 1 {
            return Err(ApplicationError::invalid(
                "target",
                "memory reads must resolve to exactly one debugger session",
            ));
        }
        let exact_thread = match &resolved.command {
            CommandTarget::Thread(thread) => Some(thread.value()),
            _ => None,
        };
        let mut threads = self
            .queries
            .threads_for_sessions(&resolved.session_ids)
            .await
            .into_iter()
            .filter(|thread| exact_thread.is_none_or(|id| thread.global_id == id))
            .collect::<Vec<_>>();
        if threads.is_empty() {
            return Err(ApplicationError::new(
                DdbErrorCode::NotReady,
                "target thread state is not available yet",
            )
            .retryable(true));
        }
        if threads.iter().any(|thread| thread.status != "stopped") {
            return Err(ApplicationError::new(
                DdbErrorCode::FailedPrecondition,
                "memory is available only while the target is stopped",
            ));
        }
        threads.sort_unstable_by_key(|thread| thread.global_id);
        Ok(StoppedTargetSnapshot(
            threads
                .into_iter()
                .map(|thread| (thread.global_id, thread.execution_revision))
                .collect(),
        ))
    }

    async fn ensure_stopped_target_current(
        &self,
        resolved: &ResolvedTarget,
        before: &StoppedTargetSnapshot,
    ) -> Result<(), ApplicationError> {
        match self.stopped_target_snapshot(resolved).await {
            Ok(after) if &after == before => Ok(()),
            _ => Err(ApplicationError::new(
                DdbErrorCode::Expired,
                "target execution changed while memory was being read",
            )
            .retryable(true)),
        }
    }

    pub(crate) async fn resolve_source(
        &self,
        request: ResolveSourceRequest,
    ) -> Result<ResolveSourceResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let location = request
            .location
            .as_ref()
            .ok_or_else(|| ApplicationError::invalid("location", "is required"))?;
        if let Some(reference) = location.source_reference.as_deref() {
            let path = self.ids.decode(ResourceIdKind::Source, reference)?;
            let source = self.source_file(reference.to_string(), path, None).await?;
            return Ok(ResolveSourceResponse {
                context: Some(scope.response_context(&self.server_instance_id)),
                source: Some(source),
            });
        }
        let reported_path = location
            .path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| ApplicationError::invalid("location.path", "is required"))?;

        let source_group_ids = self
            .queries
            .group_ids_for_source(reported_path)
            .await
            .map_err(|_| ApplicationError::backend("debugger source resolution failed"))?;
        if source_group_ids.is_empty() {
            return Err(ApplicationError::not_found("source"));
        }
        if request.target.is_some() {
            let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
                .resolve(request.target.as_ref(), TargetPurpose::Command)
                .await?;
            let belongs_to_target = self.queries.groups().into_iter().any(|group| {
                source_group_ids.contains(&group.id)
                    && group
                        .sids
                        .iter()
                        .any(|sid| resolved.session_ids.contains(sid))
            });
            if !belongs_to_target {
                return Err(ApplicationError::not_found("source for target"));
            }
        }

        let canonical = tokio::fs::canonicalize(reported_path)
            .await
            .map_err(|_| ApplicationError::not_found("source content"))?;
        let canonical = canonical
            .to_str()
            .ok_or_else(|| ApplicationError::not_found("UTF-8 source path"))?
            .to_string();
        let source_reference = self.ids.encode(ResourceIdKind::Source, &canonical)?;
        let source = self.source_file(source_reference, canonical, None).await?;
        Ok(ResolveSourceResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            source: Some(source),
        })
    }

    pub(crate) async fn read_source(
        &self,
        request: ReadSourceRequest,
    ) -> Result<ReadSourceResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        if request.start_line == 0 {
            return Err(ApplicationError::invalid(
                "start_line",
                "must be greater than zero",
            ));
        }
        if request.max_lines == 0 || request.max_lines > self.api_config.limits.max_source_lines {
            return Err(ApplicationError::invalid(
                "max_lines",
                format!(
                    "must be between 1 and {}",
                    self.api_config.limits.max_source_lines
                ),
            ));
        }
        let path = self
            .ids
            .decode(ResourceIdKind::Source, &request.source_reference)?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| ApplicationError::not_found("source content"))?;
        if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
            return Err(ApplicationError::new(
                DdbErrorCode::ResourceExhausted,
                "source content exceeds the server read bound",
            ));
        }
        let contents = tokio::fs::read_to_string(&path)
            .await
            .map_err(|_| ApplicationError::not_found("UTF-8 source content"))?;
        if contents.len() as u64 > MAX_SOURCE_BYTES {
            return Err(ApplicationError::new(
                DdbErrorCode::ResourceExhausted,
                "source content exceeds the server read bound",
            ));
        }
        let lines = contents.lines().collect::<Vec<_>>();
        let start = request.start_line.saturating_sub(1) as usize;
        let end = start
            .saturating_add(request.max_lines as usize)
            .min(lines.len());
        let selected = if start < lines.len() {
            &lines[start..end]
        } else {
            &[]
        };
        let content = selected.join("\n");
        let content_hash = format!("sha256:{:x}", Sha256::digest(contents.as_bytes()));
        let source = self
            .source_file(request.source_reference, path, Some(content_hash))
            .await?;
        Ok(ReadSourceResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            source: Some(SourceContent {
                source: Some(source),
                start_line: request.start_line,
                content,
                line_count: selected.len() as u32,
                has_more: end < lines.len(),
            }),
        })
    }

    async fn source_file(
        &self,
        source_reference: String,
        path: String,
        content_hash: Option<String>,
    ) -> Result<SourceFile, ApplicationError> {
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| ApplicationError::not_found("source content"))?;
        if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
            return Err(ApplicationError::new(
                DdbErrorCode::ResourceExhausted,
                "source content exceeds the server read bound",
            ));
        }
        let name = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ApplicationError::not_found("source name"))?
            .to_string();
        Ok(SourceFile {
            source_reference,
            path: Some(path),
            name,
            media_type: "text/plain; charset=utf-8".to_string(),
            content_hash,
        })
    }

    pub(crate) async fn list_groups(
        &self,
        request: ListGroupsRequest,
    ) -> Result<ListGroupsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let session_filter = request
            .session_id
            .as_deref()
            .map(|id| self.ids.decode(ResourceIdKind::Session, id))
            .transpose()?
            .map(|id| id.parse::<u64>())
            .transpose()
            .map_err(|_| ApplicationError::not_found("session"))?;
        let selected_session = self.queries.snapshot().await.selected_session_id;
        let projection = self.projection();
        let groups = self
            .queries
            .groups()
            .into_iter()
            .filter(|group| {
                session_filter.is_none_or(|session_id| group.sids.contains(&session_id))
            })
            .map(|group| projection.group(&group, selected_session))
            .collect::<Result<Vec<_>, _>>()?;
        let revision = collection_revision(&groups);
        let page = self
            .pages
            .paginate("groups", revision, groups, request.page.as_ref())?;
        Ok(ListGroupsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            groups: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn get_group(
        &self,
        request: GetGroupRequest,
    ) -> Result<GetGroupResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let internal_id = self
            .ids
            .decode(ResourceIdKind::Group, &request.group_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("group"))?;
        let view = self
            .queries
            .group_by_id(internal_id)
            .ok_or_else(|| ApplicationError::not_found("group"))?;
        let selected_session = self.queries.snapshot().await.selected_session_id;
        Ok(GetGroupResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            group: Some(self.projection().group(&view, selected_session)?),
        })
    }

    pub(crate) async fn list_breakpoints(
        &self,
        request: ListBreakpointsRequest,
    ) -> Result<ListBreakpointsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let target_filter = match request.target.as_ref() {
            Some(target) => {
                let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(Some(target), TargetPurpose::Breakpoint)
                    .await?;
                scope.ensure_active()?;
                Some(ResolvedTargetFilter::from_resolved(&resolved))
            }
            None => None,
        };
        let projection = self.projection();
        let breakpoints = self
            .queries
            .breakpoints()
            .iter()
            .filter(|breakpoint| {
                target_filter.as_ref().is_none_or(|filter| {
                    filter.matches(breakpoint, |group_id| {
                        self.queries.group_by_id(group_id).map(|group| group.sids)
                    })
                })
            })
            .map(|breakpoint| projection.breakpoint(breakpoint))
            .collect::<Result<Vec<_>, _>>()?;
        let revision = collection_revision(&breakpoints);
        let page =
            self.pages
                .paginate("breakpoints", revision, breakpoints, request.page.as_ref())?;
        Ok(ListBreakpointsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            breakpoints: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) fn get_breakpoint(
        &self,
        request: GetBreakpointRequest,
    ) -> Result<GetBreakpointResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let internal_id = self
            .ids
            .decode(ResourceIdKind::Breakpoint, &request.breakpoint_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("breakpoint"))?;
        let snapshot = self
            .queries
            .breakpoints()
            .into_iter()
            .find(|breakpoint| breakpoint.id == internal_id)
            .ok_or_else(|| ApplicationError::not_found("breakpoint"))?;
        Ok(GetBreakpointResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            breakpoint: Some(self.projection().breakpoint(&snapshot)?),
        })
    }

    pub(crate) fn get_operation(
        &self,
        request: GetOperationRequest,
    ) -> Result<GetOperationResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        Ok(GetOperationResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            operation: Some(self.operations.get(&request.operation_id)?),
        })
    }

    pub(crate) async fn list_operations(
        &self,
        request: ListOperationsRequest,
    ) -> Result<ListOperationsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let target_filter = match request.target.as_ref() {
            Some(target) => {
                let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(Some(target), TargetPurpose::Breakpoint)
                    .await?;
                scope.ensure_active()?;
                Some(ResolvedTargetFilter::from_resolved(&resolved))
            }
            None => None,
        };
        let kinds = parse_operation_kinds(&request.kinds)?;
        let states = parse_operation_states(&request.states)?;
        let operations = self
            .operations
            .list_with_context()
            .into_iter()
            .filter(|entry| {
                target_filter.as_ref().is_none_or(|filter| {
                    filter.matches_context(&entry.session_ids, &entry.group_ids)
                }) && (kinds.is_empty()
                    || OperationKind::try_from(entry.operation.kind)
                        .is_ok_and(|kind| kinds.contains(&kind)))
                    && (states.is_empty()
                        || OperationState::try_from(entry.operation.state)
                            .is_ok_and(|state| states.contains(&state)))
            })
            .map(|entry| entry.operation)
            .collect::<Vec<_>>();
        let revision = collection_revision(&operations);
        let page =
            self.pages
                .paginate("operations", revision, operations, request.page.as_ref())?;
        Ok(ListOperationsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            operations: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) fn list_extension_states(
        &self,
        request: ListExtensionStatesRequest,
    ) -> Result<ListExtensionStatesResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let projection = self.projection();
        let mut extension_states = self
            .queries
            .extension_states()
            .iter()
            .filter(|state| {
                request
                    .extension_id
                    .as_ref()
                    .is_none_or(|id| id == &state.extension_id)
            })
            .map(|state| projection.extension_state(state))
            .collect::<Result<Vec<_>, _>>()?;
        extension_states.sort_by(|left, right| left.extension_id.cmp(&right.extension_id));
        let revision = collection_revision(&extension_states);
        let page = self.pages.paginate(
            "extension_states",
            revision,
            extension_states,
            request.page.as_ref(),
        )?;
        Ok(ListExtensionStatesResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            extension_states: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) fn get_extension_schema(
        &self,
        request: GetExtensionSchemaRequest,
    ) -> Result<GetExtensionSchemaResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        if request.extension_id.trim().is_empty() {
            return Err(ApplicationError::invalid(
                "extension_id",
                "must not be empty",
            ));
        }
        if request.schema_uri.trim().is_empty() {
            return Err(ApplicationError::invalid("schema_uri", "must not be empty"));
        }
        let schema = self
            .queries
            .extension_schema(&request.extension_id, &request.schema_uri)
            .ok_or_else(|| ApplicationError::not_found("extension schema"))?;
        let content_sha256 = format!("{:x}", Sha256::digest(&schema.content));
        Ok(GetExtensionSchemaResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            schema: Some(ExtensionSchemaDocument {
                extension_id: request.extension_id,
                schema_uri: schema.uri,
                media_type: schema.media_type,
                content: schema.content,
                content_sha256,
            }),
        })
    }

    pub(crate) async fn list_pending_commands(
        &self,
        request: ListPendingCommandsRequest,
    ) -> Result<ListPendingCommandsResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let (session_filter, collection) = match request.target.as_ref() {
            Some(target) => {
                let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(Some(target), TargetPurpose::Command)
                    .await?;
                (
                    Some(resolved.session_ids.into_iter().collect::<HashSet<_>>()),
                    format!("{:x}", Sha256::digest(target.encode_to_vec())),
                )
            }
            None => (None, "all".to_string()),
        };
        scope.ensure_active()?;
        let projection = self.projection();
        let pending_commands = self
            .queries
            .pending_command_details()
            .iter()
            .filter(|command| {
                session_filter
                    .as_ref()
                    .is_none_or(|sessions| sessions.contains(&command.sid))
            })
            .map(|command| projection.pending_command(command))
            .collect::<Result<Vec<_>, _>>()?;
        let revision = collection_revision(&pending_commands);
        let page = self.pages.paginate(
            &format!("pending-commands:{collection}"),
            revision,
            pending_commands,
            request.page.as_ref(),
        )?;
        Ok(ListPendingCommandsResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            pending_commands: page.items,
            page: Some(page.info),
        })
    }

    pub(crate) async fn get_snapshot(
        &self,
        request: GetSnapshotRequest,
    ) -> Result<GetSnapshotResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let target_scope = match request.target.as_ref() {
            Some(target) => {
                let resolved = TargetResolver::new(self.queries.as_ref(), &self.ids)
                    .resolve(Some(target), TargetPurpose::Breakpoint)
                    .await?;
                scope.ensure_active()?;
                Some((
                    ResolvedTargetFilter::from_resolved(&resolved),
                    resolved.public,
                ))
            }
            None => None,
        };
        let sections = snapshot_sections(&request.sections)?;
        // Capture before collecting. Any later committed event is replayed from
        // this cursor even if its domain state appears in a sampled collection.
        let (cursor, base_state_revision) = self.journal.checkpoint();
        let view = self.queries.snapshot().await;
        scope.ensure_active()?;
        let projection = self.projection();
        let includes_session = |session_id| {
            target_scope
                .as_ref()
                .is_none_or(|(filter, _)| filter.includes_session(session_id))
        };
        let visible_groups = view
            .groups
            .iter()
            .filter(|group| {
                target_scope
                    .as_ref()
                    .is_none_or(|(filter, _)| filter.includes_group(group.id, &group.sids))
            })
            .cloned()
            .collect::<Vec<_>>();
        let visible_threads = view
            .threads
            .iter()
            .filter(|thread| includes_session(thread.session_id))
            .cloned()
            .collect::<Vec<_>>();
        let selected_session_id = view
            .selected_session_id
            .filter(|sid| includes_session(*sid));
        let selected_thread_id = view.selected_thread_id.filter(|thread_id| {
            visible_threads
                .iter()
                .any(|thread| thread.global_id == *thread_id)
        });

        let sessions = if sections.contains(&SnapshotSection::Topology) {
            view.sessions
                .iter()
                .filter(|session| includes_session(session.sid))
                .map(|session| projection.session(session))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let groups = if sections.contains(&SnapshotSection::Topology) {
            visible_groups
                .iter()
                .map(|group| projection.group(group, selected_session_id))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let processes = if sections.contains(&SnapshotSection::Topology) {
            view.processes
                .iter()
                .filter(|process| includes_session(process.session_id))
                .map(|process| projection.process(process))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let threads = if sections.contains(&SnapshotSection::Topology) {
            visible_threads
                .iter()
                .map(|thread| projection.thread(thread))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let selection = if sections.contains(&SnapshotSection::Selection) {
            Some(self.selection(selected_session_id, selected_thread_id, &visible_groups)?)
        } else {
            None
        };
        let execution_states = if sections.contains(&SnapshotSection::Execution) {
            match target_scope.as_ref() {
                Some((_, target)) => projection
                    .execution_state(target.clone(), &visible_threads)?
                    .map(|(_, state)| state)
                    .into_iter()
                    .collect(),
                None => self.project_execution_states(&view)?,
            }
        } else {
            Vec::new()
        };
        let breakpoints = if sections.contains(&SnapshotSection::Breakpoints) {
            view.breakpoints
                .iter()
                .filter(|breakpoint| {
                    target_scope.as_ref().is_none_or(|(filter, _)| {
                        filter.matches(breakpoint, |group_id| {
                            view.groups
                                .iter()
                                .find(|group| group.id == group_id)
                                .map(|group| group.sids.clone())
                        })
                    })
                })
                .map(|breakpoint| projection.breakpoint(breakpoint))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let operations = if sections.contains(&SnapshotSection::PendingOperations) {
            self.operations
                .list_with_context()
                .into_iter()
                .filter(|entry| {
                    target_scope.as_ref().is_none_or(|(filter, _)| {
                        filter.matches_context(&entry.session_ids, &entry.group_ids)
                    })
                })
                .map(|entry| entry.operation)
                .collect()
        } else {
            Vec::new()
        };
        let pending_commands = if sections.contains(&SnapshotSection::PendingOperations) {
            view.pending_command_details
                .iter()
                .filter(|command| includes_session(command.sid))
                .map(|command| projection.pending_command(command))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let extension_states = if sections.contains(&SnapshotSection::Extensions) {
            view.extensions
                .iter()
                .map(|state| projection.extension_state(state))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let capabilities = sections
            .contains(&SnapshotSection::Capabilities)
            .then(|| self.capabilities())
            .transpose()?;

        Ok(GetSnapshotResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            snapshot: Some(Snapshot {
                server_instance_id: self.server_instance_id.clone(),
                state_event_cursor: Some(cursor),
                base_state_revision,
                included_sections: sections.iter().map(|section| *section as i32).collect(),
                sessions,
                groups,
                processes,
                threads,
                selection,
                execution_states,
                breakpoints,
                pending_commands,
                operations,
                extension_states,
                capabilities,
            }),
        })
    }

    pub(crate) fn get_health(
        &self,
        request: GetHealthRequest,
    ) -> Result<GetHealthResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        Ok(GetHealthResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            health: Some(self.health_report(true)),
        })
    }

    pub(crate) fn get_readiness(
        &self,
        request: GetReadinessRequest,
    ) -> Result<GetReadinessResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        Ok(GetReadinessResponse {
            context: Some(scope.response_context(&self.server_instance_id)),
            readiness: Some(self.health_report(self.runtime_status.is_up())),
        })
    }

    pub(super) fn projection(&self) -> ProjectionContext<'_> {
        ProjectionContext::new(&self.ids, &self.resources, self.config.as_ref())
    }

    pub(super) fn capabilities(&self) -> Result<Capabilities, ApplicationError> {
        let descriptors = self.queries.extension_descriptors();
        let mut supported_operations = vec![
            OperationKind::Execute,
            OperationKind::SelectThread,
            OperationKind::Evaluate,
            OperationKind::CreateBreakpoint,
            OperationKind::UpdateBreakpoint,
            OperationKind::DeleteBreakpoint,
            OperationKind::RawCommand,
            OperationKind::DistributedBacktrace,
            OperationKind::Shutdown,
        ];
        if self.queries.extension_registry().has_actions() {
            supported_operations.push(OperationKind::ExtensionAction);
        }
        let mut capabilities = self.projection().capabilities(
            self.ids.encode(ResourceIdKind::Capabilities, "current")?,
            &self.server_instance_id,
            self.api_config.transports.clone(),
            self.api_config.limits,
            descriptors,
            supported_operations,
            vec![
                ExecutionAction::Continue,
                ExecutionAction::Interrupt,
                ExecutionAction::Next,
                ExecutionAction::StepIn,
                ExecutionAction::StepOut,
                ExecutionAction::Jump,
                ExecutionAction::Signal,
            ],
            vec![
                StateEventKind::ResourceUpserted,
                StateEventKind::ResourceDeleted,
                StateEventKind::SelectionChanged,
                StateEventKind::ExecutionChanged,
                StateEventKind::OperationChanged,
                StateEventKind::CapabilitiesChanged,
                StateEventKind::ExtensionStateChanged,
                StateEventKind::RequiredResync,
            ],
            vec![
                OutputStreamKind::Console,
                OutputStreamKind::Log,
                OutputStreamKind::Target,
                OutputStreamKind::InferiorStdout,
                OutputStreamKind::InferiorStderr,
                OutputStreamKind::Prompt,
            ],
            self.api_config.authentication_mode.clone(),
            0,
        );
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Capabilities,
            "current",
            &capabilities.encode_to_vec(),
        )?;
        capabilities.revision = metadata.revision;
        Ok(capabilities)
    }

    pub(super) fn selection(
        &self,
        selected_session_id: Option<u64>,
        selected_thread_id: Option<u64>,
        groups: &[crate::api::read_model::GroupView],
    ) -> Result<Selection, ApplicationError> {
        let session_id = selected_session_id
            .map(|id| self.ids.encode(ResourceIdKind::Session, id))
            .transpose()?;
        let thread_id = selected_thread_id
            .map(|id| self.ids.encode(ResourceIdKind::Thread, id))
            .transpose()?;
        let group_id = selected_session_id
            .and_then(|sid| groups.iter().find(|group| group.sids.contains(&sid)))
            .map(|group| self.ids.encode(ResourceIdKind::Group, group.id))
            .transpose()?;
        let mut selection = Selection {
            selection_id: self.ids.encode(ResourceIdKind::Selection, "current")?,
            session_id,
            group_id,
            thread_id,
            frame_id: None,
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Selection,
            "current",
            &selection.encode_to_vec(),
        )?;
        selection.revision = metadata.revision;
        Ok(selection)
    }

    fn health_report(&self, up: bool) -> HealthReport {
        let status = if up {
            HealthStatus::Up
        } else {
            HealthStatus::Down
        };
        HealthReport {
            status: status as i32,
            server_instance_id: self.server_instance_id.clone(),
            observed_at: Some(super::timestamp_now()),
            components: vec![ComponentHealth {
                component: "runtime".to_string(),
                status: status as i32,
                detail: (!up).then(|| "runtime components are still starting".to_string()),
            }],
        }
    }
}

fn output_hub_error(error: OutputHubError) -> ApplicationError {
    match error {
        OutputHubError::Closed => {
            ApplicationError::new(DdbErrorCode::Unavailable, "output service is shutting down")
                .retryable(true)
        }
        OutputHubError::SubscriberLimit => {
            ApplicationError::resource_exhausted("maximum concurrent output subscribers reached")
                .retryable(true)
        }
        OutputHubError::SequenceExhausted => {
            ApplicationError::new(DdbErrorCode::Internal, "output sequence is exhausted")
        }
    }
}

fn snapshot_sections(values: &[i32]) -> Result<Vec<SnapshotSection>, ApplicationError> {
    let defaults = [
        SnapshotSection::Topology,
        SnapshotSection::Selection,
        SnapshotSection::Breakpoints,
        SnapshotSection::PendingOperations,
        SnapshotSection::Extensions,
        SnapshotSection::Capabilities,
    ];
    let values = if values.is_empty() {
        return Ok(defaults.to_vec());
    } else {
        values
    };
    let mut seen = HashSet::new();
    let mut sections = Vec::new();
    for value in values {
        let section = SnapshotSection::try_from(*value).map_err(|_| {
            ApplicationError::invalid("sections", format!("unknown snapshot section {value}"))
        })?;
        if section == SnapshotSection::Unspecified {
            return Err(ApplicationError::invalid(
                "sections",
                "UNSPECIFIED is not a selectable snapshot section",
            ));
        }
        if seen.insert(section) {
            sections.push(section);
        }
    }
    Ok(sections)
}

fn parse_operation_kinds(values: &[i32]) -> Result<HashSet<OperationKind>, ApplicationError> {
    values
        .iter()
        .map(|value| {
            let kind = OperationKind::try_from(*value).map_err(|_| {
                ApplicationError::invalid("kinds", format!("unknown operation kind {value}"))
            })?;
            if kind == OperationKind::Unspecified {
                return Err(ApplicationError::invalid(
                    "kinds",
                    "UNSPECIFIED is not an operation-kind filter",
                ));
            }
            Ok(kind)
        })
        .collect()
}

fn parse_operation_states(values: &[i32]) -> Result<HashSet<OperationState>, ApplicationError> {
    values
        .iter()
        .map(|value| {
            let state = OperationState::try_from(*value).map_err(|_| {
                ApplicationError::invalid("states", format!("unknown operation state {value}"))
            })?;
            if state == OperationState::Unspecified {
                return Err(ApplicationError::invalid(
                    "states",
                    "UNSPECIFIED is not an operation-state filter",
                ));
            }
            Ok(state)
        })
        .collect()
}

fn validate_register_numbers(
    selected: &[(u32, String)],
    values: &HashMap<u32, String>,
) -> Result<(), ApplicationError> {
    let expected = selected
        .iter()
        .map(|(number, _)| *number)
        .collect::<HashSet<_>>();
    if values.keys().any(|number| !expected.contains(number)) {
        return Err(ApplicationError::backend(
            "debugger returned a register outside the requested bounded page",
        ));
    }
    Ok(())
}

fn encode_variable_identity(identity: &VariableIdentity) -> Result<String, ApplicationError> {
    let encoded = serde_json::to_string(identity).map_err(|_| {
        ApplicationError::new(
            DdbErrorCode::Internal,
            "variable identity could not be encoded",
        )
    })?;
    if encoded.len() > MAX_VARIABLE_IDENTITY_BYTES {
        return Err(ApplicationError::resource_exhausted(
            "variable identity exceeds the server bound",
        ));
    }
    Ok(encoded)
}

fn validate_variable_object_name(name: &str) -> Result<(), ApplicationError> {
    if name.is_empty() || name.len() > 4_096 || name.bytes().any(|byte| byte == 0) {
        return Err(ApplicationError::backend(
            "debugger returned an invalid variable-object name",
        ));
    }
    Ok(())
}

fn variable_children_command(name: &str, from: u32, to: u32) -> String {
    let name = serde_json::to_string(name)
        .expect("serializing a validated variable-object name cannot fail");
    format!("-var-list-children --all-values {name} {from} {to}")
}

fn register_unavailable(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "<unavailable>" | "<not available>" | "n/a"
    )
}

struct ResolvedTargetFilter {
    sessions: HashSet<u64>,
    groups: HashSet<u64>,
    all: bool,
}

impl ResolvedTargetFilter {
    fn from_resolved(resolved: &ResolvedTarget) -> Self {
        let mut groups = HashSet::new();
        collect_command_group_ids(&resolved.command, &mut groups);
        Self {
            sessions: resolved.session_ids.iter().copied().collect(),
            groups,
            all: matches!(resolved.command, CommandTarget::Broadcast),
        }
    }

    fn matches(
        &self,
        breakpoint: &crate::state::BreakpointSnapshot,
        group_sessions: impl Fn(u64) -> Option<Vec<u64>>,
    ) -> bool {
        if self.all {
            return true;
        }
        breakpoint.subbkpts.iter().any(|sub| match sub {
            crate::state::SubBreakpointSnapshot::Session { target_session, .. } => {
                self.sessions.contains(target_session)
            }
            crate::state::SubBreakpointSnapshot::Group { target_group, .. } => {
                self.groups.contains(target_group)
                    || group_sessions(*target_group).is_some_and(|sessions| {
                        sessions
                            .iter()
                            .any(|session| self.sessions.contains(session))
                    })
            }
        })
    }

    fn matches_context(&self, session_ids: &[u64], group_ids: &[u64]) -> bool {
        self.all
            || session_ids
                .iter()
                .any(|session| self.sessions.contains(session))
            || group_ids.iter().any(|group| self.groups.contains(group))
    }

    fn includes_session(&self, session_id: u64) -> bool {
        self.all || self.sessions.contains(&session_id)
    }

    fn includes_group(&self, group_id: u64, session_ids: &[u64]) -> bool {
        self.all
            || self.groups.contains(&group_id)
            || session_ids
                .iter()
                .any(|session| self.sessions.contains(session))
    }
}

fn collect_command_group_ids(target: &CommandTarget, groups: &mut HashSet<u64>) {
    match target {
        CommandTarget::Group(group) => {
            groups.insert(group.value());
        }
        CommandTarget::Multiple(targets) => {
            for target in targets {
                collect_command_group_ids(target, groups);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };

    use async_trait::async_trait;
    use ddb_api_extension::ExtensionProvider;
    use ddb_api_types::v2::{
        extension_payload, operation_result, resource_upsert, state_event, target, DdbErrorCode,
        ExecuteRequest, ExecutionAction, GetExtensionSchemaRequest, GetProcessRequest,
        InvokeExtensionActionRequest, ListExtensionStatesRequest, ListProcessesRequest,
        ListThreadsRequest, MultipleTarget, Operation, OperationState, PermissionScope,
        RequestContext, SessionTarget, StateEventFilter, Target as PublicTarget,
    };
    use ddb_sample_extension::{
        move_worker_payload, SampleWorkersExtension, EXTENSION_ID, MOVE_ACTION_ID, ROOT_SCHEMA_URI,
    };

    use crate::{
        api::application::{CommandPortError, PrincipalContext},
        cmd_flow::{
            api::CommandExecutor,
            router::{
                CommandFanoutReport, Router, SessionCommandFailure, SessionCommandFailureKind,
            },
            CommandOutcome, FinishedCmd, ParsedSessionResponse, Presentation,
        },
        common::config::DebuggerBackendKind,
        debugger::protocol::{Dict, Value},
        plugin::{resolve_framework_plugin, FrameworkCommandAdapter, FrameworkPlugin, GrpcAdapter},
        source::{
            catalog::SourceCatalog,
            resolver::{SourceResolutionPolicy, SourceResolver},
        },
        state::ThreadStatus,
        state::{BkptLoc, BreakpointProperties, RuntimeModel, ServiceIdentity, SubBkptSpec},
    };

    use super::*;

    #[derive(Default)]
    struct RecordingCommandPort {
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    struct HangingCommandPort;

    #[derive(Debug)]
    struct SampleExtensionPlugin;

    impl FrameworkPlugin for SampleExtensionPlugin {
        fn command_adapter(&self) -> Arc<dyn FrameworkCommandAdapter> {
            Arc::new(GrpcAdapter)
        }

        fn api_extensions(
            &self,
            _config: &Config,
            _model: Arc<RuntimeModel>,
        ) -> Vec<Arc<dyn ExtensionProvider>> {
            vec![Arc::new(SampleWorkersExtension::default())]
        }
    }

    struct PartialCommandPort {
        report: CommandFanoutReport,
    }

    #[async_trait]
    impl ApplicationCommandPort for HangingCommandPort {
        async fn execute(
            &self,
            _command: &str,
            _target: crate::cmd_flow::router::Target,
        ) -> Result<CommandOutcome, CommandPortError> {
            std::future::pending().await
        }
    }

    struct SignalCommandPort;

    #[async_trait]
    impl ApplicationCommandPort for SignalCommandPort {
        async fn execute(
            &self,
            command: &str,
            target: crate::cmd_flow::router::Target,
        ) -> Result<CommandOutcome, CommandPortError> {
            assert_eq!(command, "-list-signals");
            assert_eq!(target, crate::cmd_flow::router::Target::Session(42));
            let signal = |name: &str, stop: &str, pass: &str| {
                Value::Dict(
                    vec![
                        ("name".to_string(), name.into()),
                        ("stop".to_string(), stop.into()),
                        ("print".to_string(), "Yes".into()),
                        ("pass".to_string(), pass.into()),
                        (
                            "description".to_string(),
                            format!("{name} description").into(),
                        ),
                    ]
                    .into(),
                )
            };
            let payload: Dict = vec![(
                "signals".to_string(),
                Value::List(vec![
                    signal("SIGINT", "Yes", "No"),
                    signal("SIGUSR1", "No", "Yes"),
                ]),
            )]
            .into();
            Ok(CommandOutcome::response(
                FinishedCmd::new(
                    None,
                    42,
                    vec![ParsedSessionResponse::new(
                        42,
                        "done".to_string(),
                        Some(payload),
                    )],
                ),
                Presentation::Plain,
            ))
        }
    }

    #[async_trait]
    impl ApplicationCommandPort for PartialCommandPort {
        async fn execute(
            &self,
            _command: &str,
            _target: crate::cmd_flow::router::Target,
        ) -> Result<CommandOutcome, CommandPortError> {
            Err(CommandPortError::test_with_fanout(self.report.clone()))
        }
    }

    #[async_trait]
    impl ApplicationCommandPort for RecordingCommandPort {
        async fn execute(
            &self,
            _command: &str,
            _target: crate::cmd_flow::router::Target,
        ) -> Result<CommandOutcome, CommandPortError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                Err(CommandPortError::test(
                    "sensitive backend failure must not cross the API boundary",
                ))
            } else {
                Ok(CommandOutcome::empty())
            }
        }
    }

    fn service(model: Arc<RuntimeModel>) -> Arc<DdbApplicationService> {
        service_with_port(model, Arc::new(super::super::NoopCommandPort))
    }

    fn service_with_port(
        model: Arc<RuntimeModel>,
        command_port: Arc<dyn ApplicationCommandPort>,
    ) -> Arc<DdbApplicationService> {
        service_with_port_and_output(
            model,
            command_port,
            crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
        )
    }

    fn service_with_port_and_output(
        model: Arc<RuntimeModel>,
        command_port: Arc<dyn ApplicationCommandPort>,
        output: Arc<crate::cmd_flow::output_hub::OutputHub>,
    ) -> Arc<DdbApplicationService> {
        let mut config = Config::default();
        config.conf.debugger.backend = DebuggerBackendKind::Mock;
        let config = Arc::new(config);
        let plugin = resolve_framework_plugin(config.as_ref());
        service_with_components(model, command_port, output, config, plugin)
    }

    fn service_with_plugin(
        model: Arc<RuntimeModel>,
        plugin: Arc<dyn FrameworkPlugin>,
    ) -> Arc<DdbApplicationService> {
        let mut config = Config::default();
        config.conf.debugger.backend = DebuggerBackendKind::Mock;
        service_with_components(
            model,
            Arc::new(super::super::NoopCommandPort),
            crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
            Arc::new(config),
            plugin,
        )
    }

    fn service_with_components(
        model: Arc<RuntimeModel>,
        command_port: Arc<dyn ApplicationCommandPort>,
        output: Arc<crate::cmd_flow::output_hub::OutputHub>,
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
    ) -> Arc<DdbApplicationService> {
        let router = Arc::new(Router::new(Arc::clone(&model)));
        let resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::clone(&model) as _,
            CommandExecutor::new(Arc::clone(&router)),
            SourceResolutionPolicy::OnDemand,
        );
        let queries =
            ApiQueries::new(model, router, resolver, Arc::clone(&config), plugin).unwrap();
        let resource_limits = config.conf.api_limits.clone();
        DdbApplicationService::new(
            queries,
            command_port,
            output,
            config,
            Arc::new(RuntimeStatus::new()),
            Arc::new(ShutdownCtrl::new()),
            DdbApplicationConfig::http(
                "/api/v2",
                false,
                "none-insecure-development",
                &resource_limits,
            ),
        )
    }

    fn session_target(session_id: impl Into<String>) -> PublicTarget {
        PublicTarget {
            selector: Some(target::Selector::Session(SessionTarget {
                session_id: session_id.into(),
            })),
        }
    }

    fn execute_request(
        idempotency_key: &str,
        target: PublicTarget,
        action: ExecutionAction,
    ) -> ExecuteRequest {
        ExecuteRequest {
            context: Some(RequestContext {
                idempotency_key: Some(idempotency_key.to_string()),
                ..RequestContext::default()
            }),
            target: Some(target),
            action: action as i32,
            ..ExecuteRequest::default()
        }
    }

    async fn next_operation_event(
        subscription: &mut ApplicationStateSubscription,
        operation_id: &str,
    ) -> Operation {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = subscription
                    .recv()
                    .await
                    .expect("state journal should remain open");
                let Some(state_event::Payload::Upsert(upsert)) = event.payload else {
                    continue;
                };
                let Some(resource_upsert::Resource::Operation(operation)) = upsert.resource else {
                    continue;
                };
                if operation.operation_id == operation_id {
                    return operation;
                }
            }
        })
        .await
        .expect("operation event should arrive")
    }

    async fn next_resource_event(
        subscription: &mut ApplicationStateSubscription,
        event_kind: StateEventKind,
        resource_kind: ResourceKind,
        resource_id: &str,
    ) -> StateEvent {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = subscription
                    .recv()
                    .await
                    .expect("state journal should remain open");
                if event.kind == event_kind as i32
                    && event.resource_kind == resource_kind as i32
                    && event.resource_id == resource_id
                {
                    return event;
                }
            }
        })
        .await
        .expect("resource event should arrive")
    }

    async fn public_session_id(service: &DdbApplicationService) -> String {
        service
            .list_sessions(ListSessionsRequest::default())
            .await
            .expect("session listing should succeed")
            .sessions
            .into_iter()
            .next()
            .expect("one session should exist")
            .session_id
    }

    #[tokio::test]
    async fn response_size_validation_enforces_advertised_boundary() {
        let service = service(RuntimeModel::new());
        let maximum = service.api_config.limits.max_response_bytes as usize;

        service.validate_response_bytes(maximum).unwrap();
        let error = service.validate_response_bytes(maximum + 1).unwrap_err();
        assert_eq!(error.code(), DdbErrorCode::ResourceExhausted);
    }

    #[tokio::test]
    async fn session_and_group_ids_are_opaque_stable_and_round_trip() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        let identity = ServiceIdentity::new("worker-hash", "worker");
        drop(model.register_service_group(42, &identity).await);
        model.complete_session_activation(42, None).await;
        let service = service(model);

        let listed = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap();
        let session = listed.sessions.into_iter().next().unwrap();
        assert!(session.session_id.starts_with("ses_"));
        assert_ne!(session.session_id, "ses_42");
        assert_eq!(
            service
                .get_session(GetSessionRequest {
                    session_id: session.session_id.clone(),
                    ..GetSessionRequest::default()
                })
                .await
                .unwrap()
                .session
                .unwrap()
                .session_id,
            session.session_id
        );

        let groups = service
            .list_groups(ListGroupsRequest::default())
            .await
            .unwrap();
        assert_eq!(groups.groups.len(), 1);
        assert!(groups.groups[0].group_id.starts_with("grp_"));
    }

    #[tokio::test]
    async fn runtime_session_changes_publish_upserts_and_exact_tombstones() {
        let model = RuntimeModel::new();
        let service = service(Arc::clone(&model));
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        model.register_session(42, "worker", None).await;
        let initial = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .remove(0);
        let created = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Session,
            &initial.session_id,
        )
        .await;
        assert_eq!(created.resource_revision, initial.revision);
        let state_event::Payload::Upsert(created) = created.payload.unwrap() else {
            panic!("session creation must carry a typed upsert");
        };
        let resource_upsert::Resource::Session(created) = created.resource.unwrap() else {
            panic!("session event must carry a session");
        };
        assert_eq!(created, initial);

        model.complete_session_activation(42, None).await;
        let active = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Session,
            &initial.session_id,
        )
        .await;
        assert!(active.resource_revision > initial.revision);

        drop(model.begin_session_retirement(42).await.finish().await);
        let deleted = next_resource_event(
            &mut events,
            StateEventKind::ResourceDeleted,
            ResourceKind::Session,
            &initial.session_id,
        )
        .await;
        assert!(deleted.resource_revision > active.resource_revision);
        let state_event::Payload::Deleted(tombstone) = deleted.payload.unwrap() else {
            panic!("session retirement must carry a typed tombstone");
        };
        assert_eq!(tombstone.resource_id, initial.session_id);
        assert_eq!(tombstone.resource_revision, deleted.resource_revision);
        assert!(service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .is_empty());
    }

    #[tokio::test]
    async fn scoped_state_subscription_observes_selection_moving_out_of_scope() {
        let model = RuntimeModel::new();
        for (session_id, local_thread_id, group, name) in
            [(42, 11, "i42", "worker-42"), (43, 12, "i43", "worker-43")]
        {
            model.register_session(session_id, name, None).await;
            model.complete_session_activation(session_id, None).await;
            model
                .register_thread_group(session_id, group)
                .await
                .unwrap();
            model
                .register_thread(session_id, local_thread_id, group)
                .await
                .unwrap();
        }
        let service = service(Arc::clone(&model));
        let session_42 = service.ids.encode(ResourceIdKind::Session, 42).unwrap();
        let session_43 = service.ids.encode(ResourceIdKind::Session, 43).unwrap();
        let mut all_events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        model.select_local_thread(42, 11).await.unwrap();
        let selected_in_42 = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = all_events.recv().await.unwrap();
                let Some(state_event::Payload::Upsert(upsert)) = event.payload.as_ref() else {
                    continue;
                };
                let Some(resource_upsert::Resource::Selection(selection)) =
                    upsert.resource.as_ref()
                else {
                    continue;
                };
                if selection.session_id.as_deref() == Some(session_42.as_str()) {
                    return event;
                }
            }
        })
        .await
        .expect("selection into session 42 should be projected");

        let mut scoped = service
            .subscribe_state_events(SubscribeStateEventsRequest {
                after_cursor: selected_in_42.cursor.clone(),
                filter: Some(StateEventFilter {
                    kinds: vec![StateEventKind::SelectionChanged as i32],
                    resource_kinds: vec![ResourceKind::Selection as i32],
                    session_ids: vec![session_42.clone()],
                    ..StateEventFilter::default()
                }),
                ..SubscribeStateEventsRequest::default()
            })
            .unwrap();

        model.select_local_thread(43, 12).await.unwrap();
        let moved = tokio::time::timeout(Duration::from_secs(2), scoped.recv())
            .await
            .expect("moving selection out of a filtered session must remain observable")
            .expect("state journal should remain open");
        let state_event::Payload::Upsert(upsert) = moved.payload.unwrap() else {
            panic!("selection change must carry an upsert");
        };
        let resource_upsert::Resource::Selection(selection) = upsert.resource.unwrap() else {
            panic!("selection change must carry a selection");
        };
        assert_eq!(selection.session_id.as_deref(), Some(session_43.as_str()));
    }

    #[tokio::test]
    async fn scoped_state_subscription_replays_session_tombstones() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        let service = service(Arc::clone(&model));
        let session_id = service.ids.encode(ResourceIdKind::Session, 42).unwrap();
        let mut live = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        model.complete_session_activation(42, None).await;
        let before_delete = next_resource_event(
            &mut live,
            StateEventKind::ResourceUpserted,
            ResourceKind::Session,
            &session_id,
        )
        .await;
        drop(model.begin_session_retirement(42).await.finish().await);
        let deleted = next_resource_event(
            &mut live,
            StateEventKind::ResourceDeleted,
            ResourceKind::Session,
            &session_id,
        )
        .await;

        let mut replay = service
            .subscribe_state_events(SubscribeStateEventsRequest {
                after_cursor: before_delete.cursor,
                filter: Some(StateEventFilter {
                    kinds: vec![StateEventKind::ResourceDeleted as i32],
                    resource_kinds: vec![ResourceKind::Session as i32],
                    session_ids: vec![session_id],
                    ..StateEventFilter::default()
                }),
                ..SubscribeStateEventsRequest::default()
            })
            .unwrap();
        let replayed = tokio::time::timeout(Duration::from_secs(2), replay.recv())
            .await
            .expect("retained scoped tombstone should replay")
            .expect("state journal should remain open");
        assert_eq!(replayed.cursor, deleted.cursor);
        assert_eq!(replayed.kind, StateEventKind::ResourceDeleted as i32);
    }

    #[tokio::test]
    async fn thread_targeted_operations_are_visible_to_owning_session_subscribers() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        model.register_thread_group(42, "i1").await.unwrap();
        model.register_thread(42, 11, "i1").await.unwrap();
        let service = service_with_port(model, Arc::new(RecordingCommandPort::default()));
        let session_id = service.ids.encode(ResourceIdKind::Session, 42).unwrap();
        let thread_id = service
            .list_threads(ListThreadsRequest {
                target: Some(session_target(session_id.clone())),
                ..ListThreadsRequest::default()
            })
            .await
            .unwrap()
            .threads
            .remove(0)
            .thread_id;
        let mut scoped = service
            .subscribe_state_events(SubscribeStateEventsRequest {
                filter: Some(StateEventFilter {
                    resource_kinds: vec![ResourceKind::Operation as i32],
                    session_ids: vec![session_id],
                    ..StateEventFilter::default()
                }),
                ..SubscribeStateEventsRequest::default()
            })
            .unwrap();
        let principal = PrincipalContext::new("principal-a").unwrap();
        let operation = service
            .execute(
                &principal,
                execute_request(
                    "thread-operation",
                    PublicTarget {
                        selector: Some(target::Selector::Thread(ddb_api_types::v2::ThreadTarget {
                            thread_id,
                        })),
                    },
                    ExecutionAction::Continue,
                ),
            )
            .await
            .unwrap()
            .operation
            .unwrap();

        let observed = next_operation_event(&mut scoped, &operation.operation_id).await;
        assert_eq!(observed.operation_id, operation.operation_id);
    }

    #[tokio::test]
    async fn debugger_reads_do_not_wait_past_the_request_deadline() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        model.register_thread_group(42, "i1").await.unwrap();
        model.register_thread(42, 11, "i1").await.unwrap();
        model
            .update_thread_statuses(42, &[11], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let service = service_with_port(model, Arc::new(HangingCommandPort));
        let session_id = service.ids.encode(ResourceIdKind::Session, 42).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            service.read_memory(ReadMemoryRequest {
                context: Some(RequestContext {
                    deadline: Some(super::super::timestamp_after(Duration::from_millis(20))),
                    ..RequestContext::default()
                }),
                target: Some(session_target(session_id)),
                address: "0x1000".to_string(),
                byte_count: 1,
            }),
        )
        .await
        .expect("application deadline must bound the backend wait")
        .unwrap_err();
        assert_eq!(result.code(), DdbErrorCode::DeadlineExceeded);
    }

    #[tokio::test]
    async fn runtime_topology_selection_and_breakpoint_paths_are_event_complete() {
        let model = RuntimeModel::new();
        let service = service(Arc::clone(&model));
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        model.register_session(42, "worker", None).await;
        let session = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .remove(0);
        let _ = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Session,
            &session.session_id,
        )
        .await;

        let identity = ServiceIdentity::new("worker-hash", "workers");
        drop(model.register_service_group(42, &identity).await);
        let group = service
            .list_groups(ListGroupsRequest::default())
            .await
            .unwrap()
            .groups
            .remove(0);
        let group_created = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Group,
            &group.group_id,
        )
        .await;
        assert_eq!(group_created.resource_revision, group.revision);
        model.complete_session_activation(42, None).await;

        model.register_thread_group(42, "i1").await.unwrap();
        let process = service
            .list_processes(ListProcessesRequest::default())
            .await
            .unwrap()
            .processes
            .remove(0);
        let process_created = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Process,
            &process.process_id,
        )
        .await;
        assert_eq!(process_created.resource_revision, process.revision);

        model.start_thread_group(42, "i1", 9_001).await.unwrap();
        let process_started = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Process,
            &process.process_id,
        )
        .await;
        assert!(process_started.resource_revision > process_created.resource_revision);

        model.register_thread(42, 11, "i1").await.unwrap();
        let target = session_target(session.session_id.clone());
        let thread = service
            .list_threads(ListThreadsRequest {
                target: Some(target),
                ..ListThreadsRequest::default()
            })
            .await
            .unwrap()
            .threads
            .remove(0);
        let thread_created = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Thread,
            &thread.thread_id,
        )
        .await;
        assert_eq!(thread_created.resource_revision, thread.revision);

        let selection = service
            .get_snapshot(GetSnapshotRequest::default())
            .await
            .unwrap()
            .snapshot
            .unwrap()
            .selection
            .unwrap();
        model.select_local_thread(42, 11).await.unwrap();
        let selected = next_resource_event(
            &mut events,
            StateEventKind::SelectionChanged,
            ResourceKind::Selection,
            &selection.selection_id,
        )
        .await;
        assert!(selected.resource_revision > selection.revision);

        model
            .update_thread_statuses(42, &[11], ThreadStatus::RUNNING)
            .await
            .unwrap();
        let thread_target = PublicTarget {
            selector: Some(target::Selector::Thread(ddb_api_types::v2::ThreadTarget {
                thread_id: thread.thread_id.clone(),
            })),
        };
        let execution = service
            .get_execution_state(GetExecutionStateRequest {
                target: Some(thread_target),
                ..GetExecutionStateRequest::default()
            })
            .await
            .unwrap()
            .execution_state
            .unwrap();
        let running = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Thread,
            &thread.thread_id,
        )
        .await;
        assert!(running.resource_revision > thread_created.resource_revision);
        let execution_changed = next_resource_event(
            &mut events,
            StateEventKind::ExecutionChanged,
            ResourceKind::ExecutionState,
            &execution.execution_state_id,
        )
        .await;
        assert_eq!(execution_changed.resource_revision, execution.revision);
        let execution_snapshot = service
            .get_snapshot(GetSnapshotRequest {
                sections: vec![SnapshotSection::Execution as i32],
                ..GetSnapshotRequest::default()
            })
            .await
            .unwrap()
            .snapshot
            .unwrap();
        assert_eq!(execution_snapshot.execution_states.len(), 4);
        assert!(execution_snapshot
            .execution_states
            .iter()
            .all(|state| state.running));
        assert!(execution_snapshot
            .execution_states
            .iter()
            .any(|state| state.execution_state_id == execution.execution_state_id));

        model.remove_thread(42, 11, "i1").await.unwrap();
        let thread_deleted = next_resource_event(
            &mut events,
            StateEventKind::ResourceDeleted,
            ResourceKind::Thread,
            &thread.thread_id,
        )
        .await;
        assert!(thread_deleted.resource_revision > running.resource_revision);
        let execution_deleted = next_resource_event(
            &mut events,
            StateEventKind::ResourceDeleted,
            ResourceKind::ExecutionState,
            &execution.execution_state_id,
        )
        .await;
        assert!(execution_deleted.resource_revision > execution.revision);

        model.remove_thread_group(42, "i1").await.unwrap();
        let process_deleted = next_resource_event(
            &mut events,
            StateEventKind::ResourceDeleted,
            ResourceKind::Process,
            &process.process_id,
        )
        .await;
        assert!(process_deleted.resource_revision > process_started.resource_revision);

        model
            .insert_breakpoint(
                BkptLoc::new("src/main.rs", 12),
                BreakpointProperties::default(),
                vec![SubBkptSpec::Session {
                    sid: 42,
                    local_id: 7,
                }],
            )
            .unwrap();
        let breakpoint = service
            .list_breakpoints(ListBreakpointsRequest::default())
            .await
            .unwrap()
            .breakpoints
            .remove(0);
        let breakpoint_created = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Breakpoint,
            &breakpoint.breakpoint_id,
        )
        .await;
        assert_eq!(breakpoint_created.resource_revision, breakpoint.revision);

        model.record_breakpoint_hit(42, 7).unwrap();
        let breakpoint_hit = next_resource_event(
            &mut events,
            StateEventKind::ResourceUpserted,
            ResourceKind::Breakpoint,
            &breakpoint.breakpoint_id,
        )
        .await;
        assert!(breakpoint_hit.resource_revision > breakpoint_created.resource_revision);

        let internal_breakpoint = service
            .ids
            .decode(ResourceIdKind::Breakpoint, &breakpoint.breakpoint_id)
            .unwrap()
            .parse::<u64>()
            .unwrap();
        model.remove_breakpoint(internal_breakpoint);
        let breakpoint_deleted = next_resource_event(
            &mut events,
            StateEventKind::ResourceDeleted,
            ResourceKind::Breakpoint,
            &breakpoint.breakpoint_id,
        )
        .await;
        assert!(breakpoint_deleted.resource_revision > breakpoint_hit.resource_revision);
    }

    #[tokio::test]
    async fn session_and_group_revisions_track_only_visible_state_changes() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker-a", None).await;
        let identity = ServiceIdentity::new("worker-hash", "workers");
        drop(model.register_service_group(42, &identity).await);
        let service = service(Arc::clone(&model));

        let initial_session = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .remove(0);
        let repeated_session = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .remove(0);
        assert_eq!(repeated_session, initial_session);

        model.complete_session_activation(42, None).await;
        let active_session = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .remove(0);
        assert_ne!(active_session.status, initial_session.status);
        assert_eq!(active_session.revision, initial_session.revision + 1);
        assert_eq!(
            service
                .list_sessions(ListSessionsRequest::default())
                .await
                .unwrap()
                .sessions
                .remove(0),
            active_session
        );

        let initial_group = service
            .list_groups(ListGroupsRequest::default())
            .await
            .unwrap()
            .groups
            .remove(0);
        assert_eq!(
            service
                .list_groups(ListGroupsRequest::default())
                .await
                .unwrap()
                .groups
                .remove(0),
            initial_group
        );

        model.register_session(43, "worker-b", None).await;
        drop(model.register_service_group(43, &identity).await);
        let expanded_group = service
            .list_groups(ListGroupsRequest::default())
            .await
            .unwrap()
            .groups
            .remove(0);
        assert_eq!(expanded_group.session_ids.len(), 2);
        assert_eq!(expanded_group.revision, initial_group.revision + 1);
        assert_eq!(
            service
                .list_groups(ListGroupsRequest::default())
                .await
                .unwrap()
                .groups
                .remove(0),
            expanded_group
        );
    }

    #[tokio::test]
    async fn thread_and_selection_revisions_track_selection_changes() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        model.register_thread_group(42, "i1").await.unwrap();
        model.start_thread_group(42, "i1", 9_001).await.unwrap();
        model.register_thread(42, 11, "i1").await.unwrap();
        model.register_thread(42, 22, "i1").await.unwrap();
        let service = service(Arc::clone(&model));
        let target = session_target(public_session_id(&service).await);

        let before_threads = service
            .list_threads(ListThreadsRequest {
                target: Some(target.clone()),
                ..ListThreadsRequest::default()
            })
            .await
            .unwrap()
            .threads;
        assert_eq!(before_threads.len(), 2);
        assert!(before_threads.iter().all(|thread| !thread.selected));
        assert_eq!(
            service
                .list_threads(ListThreadsRequest {
                    target: Some(target.clone()),
                    ..ListThreadsRequest::default()
                })
                .await
                .unwrap()
                .threads,
            before_threads
        );
        let before_selection = service
            .get_snapshot(GetSnapshotRequest::default())
            .await
            .unwrap()
            .snapshot
            .unwrap()
            .selection
            .unwrap();
        assert!(before_selection.selection_id.starts_with("sel_"));
        assert!(before_selection.thread_id.is_none());

        model.select_local_thread(42, 22).await.unwrap();
        let after_threads = service
            .list_threads(ListThreadsRequest {
                target: Some(target.clone()),
                ..ListThreadsRequest::default()
            })
            .await
            .unwrap()
            .threads;
        let selected = after_threads
            .iter()
            .find(|thread| thread.selected)
            .expect("selected thread must be visible");
        let previous = before_threads
            .iter()
            .find(|thread| thread.thread_id == selected.thread_id)
            .unwrap();
        assert_eq!(selected.revision, previous.revision + 1);
        let unselected = after_threads
            .iter()
            .find(|thread| !thread.selected)
            .unwrap();
        let previous_unselected = before_threads
            .iter()
            .find(|thread| thread.thread_id == unselected.thread_id)
            .unwrap();
        assert_eq!(unselected.revision, previous_unselected.revision);
        assert_eq!(
            service
                .list_threads(ListThreadsRequest {
                    target: Some(target),
                    ..ListThreadsRequest::default()
                })
                .await
                .unwrap()
                .threads,
            after_threads
        );

        let after_selection = service
            .get_snapshot(GetSnapshotRequest::default())
            .await
            .unwrap()
            .snapshot
            .unwrap()
            .selection
            .unwrap();
        assert_eq!(after_selection.selection_id, before_selection.selection_id);
        assert_eq!(
            after_selection.thread_id.as_deref(),
            Some(selected.thread_id.as_str())
        );
        assert_eq!(after_selection.revision, before_selection.revision + 1);
        assert_eq!(
            service
                .get_snapshot(GetSnapshotRequest::default())
                .await
                .unwrap()
                .snapshot
                .unwrap()
                .selection
                .unwrap(),
            after_selection
        );
    }

    #[tokio::test]
    async fn breakpoint_and_capability_revisions_are_content_driven() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        model
            .insert_breakpoint(
                BkptLoc::new("src/main.rs", 12),
                BreakpointProperties::default(),
                vec![SubBkptSpec::Session {
                    sid: 42,
                    local_id: 7,
                }],
            )
            .unwrap();
        let service = service(Arc::clone(&model));

        let initial = service
            .list_breakpoints(ListBreakpointsRequest::default())
            .await
            .unwrap()
            .breakpoints
            .remove(0);
        assert_eq!(
            service
                .list_breakpoints(ListBreakpointsRequest::default())
                .await
                .unwrap()
                .breakpoints
                .remove(0),
            initial
        );

        model.record_breakpoint_hit(42, 7).unwrap();
        let hit = service
            .list_breakpoints(ListBreakpointsRequest::default())
            .await
            .unwrap()
            .breakpoints
            .remove(0);
        assert_eq!(hit.hit_count, initial.hit_count + 1);
        assert_eq!(hit.revision, initial.revision + 1);
        assert_eq!(hit.sub_breakpoints, initial.sub_breakpoints);
        assert_eq!(
            service
                .list_breakpoints(ListBreakpointsRequest::default())
                .await
                .unwrap()
                .breakpoints
                .remove(0),
            hit
        );

        let capabilities = service
            .get_capabilities(GetCapabilitiesRequest::default())
            .await
            .unwrap()
            .capabilities
            .unwrap();
        assert_eq!(
            service
                .get_capabilities(GetCapabilitiesRequest::default())
                .await
                .unwrap()
                .capabilities
                .unwrap(),
            capabilities
        );
        assert_eq!(
            capabilities.output_stream_kinds,
            vec![
                OutputStreamKind::Console as i32,
                OutputStreamKind::Log as i32,
                OutputStreamKind::Target as i32,
                OutputStreamKind::InferiorStdout as i32,
                OutputStreamKind::InferiorStderr as i32,
                OutputStreamKind::Prompt as i32,
            ]
        );
        let target_capabilities = service
            .get_capabilities(GetCapabilitiesRequest {
                target: Some(session_target(public_session_id(&service).await)),
                ..GetCapabilitiesRequest::default()
            })
            .await
            .unwrap()
            .capabilities
            .unwrap();
        assert_eq!(target_capabilities, capabilities);
    }

    #[tokio::test]
    async fn capabilities_are_a_replayable_resource_and_advertise_the_complete_state_lane() {
        let service = service(RuntimeModel::new());
        let capabilities_id = service
            .ids
            .encode(ResourceIdKind::Capabilities, "current")
            .unwrap();
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest {
                after_cursor: Some(Cursor {
                    server_instance_id: service.server_instance_id().to_string(),
                    sequence: 0,
                }),
                filter: Some(StateEventFilter {
                    kinds: vec![StateEventKind::CapabilitiesChanged as i32],
                    resource_kinds: vec![ResourceKind::Capabilities as i32],
                    ..StateEventFilter::default()
                }),
                ..SubscribeStateEventsRequest::default()
            })
            .unwrap();

        let event = next_resource_event(
            &mut events,
            StateEventKind::CapabilitiesChanged,
            ResourceKind::Capabilities,
            &capabilities_id,
        )
        .await;
        let state_event::Payload::Upsert(upsert) = event.payload.unwrap() else {
            panic!("capability change should be a resource upsert");
        };
        let resource_upsert::Resource::Capabilities(capabilities) = upsert.resource.unwrap() else {
            panic!("capability event should carry the complete capability document");
        };
        assert_eq!(event.resource_revision, capabilities.revision);
        assert_eq!(
            capabilities.state_event_kinds,
            vec![
                StateEventKind::ResourceUpserted as i32,
                StateEventKind::ResourceDeleted as i32,
                StateEventKind::SelectionChanged as i32,
                StateEventKind::ExecutionChanged as i32,
                StateEventKind::OperationChanged as i32,
                StateEventKind::CapabilitiesChanged as i32,
                StateEventKind::ExtensionStateChanged as i32,
                StateEventKind::RequiredResync as i32,
            ]
        );
        assert_eq!(
            service
                .get_capabilities(GetCapabilitiesRequest::default())
                .await
                .unwrap()
                .capabilities,
            Some(capabilities)
        );
    }

    #[tokio::test]
    async fn breakpoint_target_filters_include_direct_and_group_inherited_effects() {
        let model = RuntimeModel::new();
        let identity = ServiceIdentity::new("workers-hash", "workers");
        for sid in [42, 43] {
            model.register_session(sid, "worker", None).await;
            drop(model.register_service_group(sid, &identity).await);
            model.complete_session_activation(sid, None).await;
        }
        let group_id = model.group_id_by_session(42).unwrap();
        model
            .insert_breakpoint(
                BkptLoc::new("src/direct.rs", 12),
                BreakpointProperties::default(),
                vec![SubBkptSpec::Session {
                    sid: 42,
                    local_id: 1,
                }],
            )
            .unwrap();
        model
            .insert_breakpoint(
                BkptLoc::new("src/group.rs", 18),
                BreakpointProperties::default(),
                vec![SubBkptSpec::Group {
                    group_id,
                    locals: vec![(42, 2), (43, 3)],
                }],
            )
            .unwrap();
        let service = service(model);
        let session_42 = service.ids.encode(ResourceIdKind::Session, 42).unwrap();
        let session_43 = service.ids.encode(ResourceIdKind::Session, 43).unwrap();
        let group = service
            .list_groups(ListGroupsRequest::default())
            .await
            .unwrap()
            .groups
            .remove(0);

        let for_42 = service
            .list_breakpoints(ListBreakpointsRequest {
                target: Some(session_target(session_42)),
                ..ListBreakpointsRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(for_42.breakpoints.len(), 2);

        let for_43 = service
            .list_breakpoints(ListBreakpointsRequest {
                target: Some(session_target(session_43)),
                ..ListBreakpointsRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(for_43.breakpoints.len(), 1);
        assert_eq!(
            for_43.breakpoints[0]
                .spec
                .as_ref()
                .and_then(|spec| spec.location.as_ref()),
            Some(&ddb_api_types::v2::breakpoint_spec::Location::Source(
                ddb_api_types::v2::SourceBreakpointLocation {
                    source: "src/group.rs".to_string(),
                    line: 18,
                    column: 0,
                }
            ))
        );

        let for_group = service
            .list_breakpoints(ListBreakpointsRequest {
                target: Some(PublicTarget {
                    selector: Some(target::Selector::Group(ddb_api_types::v2::GroupTarget {
                        group_id: group.group_id,
                    })),
                }),
                ..ListBreakpointsRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(for_group.breakpoints.len(), 2);
    }

    #[tokio::test]
    async fn output_subscriptions_filter_and_report_unavailable_replay_explicitly() {
        use crate::cmd_flow::output_hub::{DebuggerOutputStream, OutputHub};

        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        let output = OutputHub::new(Default::default());
        let service = service_with_port_and_output(
            Arc::clone(&model),
            Arc::new(super::super::NoopCommandPort),
            Arc::clone(&output),
        );
        let session_id = public_session_id(&service).await;
        let mut subscription = service
            .subscribe_output(SubscribeOutputRequest {
                filter: Some(ddb_api_types::v2::OutputFilter {
                    streams: vec![OutputStreamKind::Console as i32],
                    session_ids: vec![session_id.clone()],
                    thread_ids: Vec::new(),
                }),
                ..SubscribeOutputRequest::default()
            })
            .unwrap();

        output
            .publish(Some(42), DebuggerOutputStream::Log, "filtered")
            .unwrap();
        output
            .publish(Some(42), DebuggerOutputStream::Console, "visible")
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), subscription.recv())
            .await
            .expect("matching output should be delivered")
            .expect("output hub should remain open");
        assert_eq!(event.session_id.as_deref(), Some(session_id.as_str()));
        assert_eq!(event.stream, OutputStreamKind::Console as i32);
        assert_eq!(event.cursor.as_ref().unwrap().sequence, 2);
        assert_eq!(
            event.content,
            Some(ddb_api_types::v2::output_event::Content::Text(
                "visible".to_string()
            ))
        );
        assert!(event.gap.is_none());

        let mut resumed = service
            .subscribe_output(SubscribeOutputRequest {
                after_cursor: Some(Cursor {
                    server_instance_id: service.server_instance_id.clone(),
                    sequence: 0,
                }),
                ..SubscribeOutputRequest::default()
            })
            .unwrap();
        let gap_event = resumed.recv().await.unwrap();
        let gap = gap_event.gap.expect("unretained output must be explicit");
        assert_eq!(gap.first_missing_sequence, 1);
        assert_eq!(gap.last_missing_sequence, 2);
        assert_eq!(gap.dropped_events, Some(2));
        assert_eq!(gap_event.cursor.unwrap().sequence, 2);

        let foreign_cursor = service
            .subscribe_output(SubscribeOutputRequest {
                after_cursor: Some(Cursor {
                    server_instance_id: "another-server".to_string(),
                    sequence: 0,
                }),
                ..SubscribeOutputRequest::default()
            })
            .err()
            .expect("a foreign cursor must be rejected");
        assert_eq!(foreign_cursor.code(), DdbErrorCode::ReplayGap);

        let future_cursor = service
            .subscribe_output(SubscribeOutputRequest {
                after_cursor: Some(Cursor {
                    server_instance_id: service.server_instance_id.clone(),
                    sequence: 3,
                }),
                ..SubscribeOutputRequest::default()
            })
            .err()
            .expect("a future cursor must be rejected");
        assert_eq!(future_cursor.code(), DdbErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn process_resources_preserve_inferior_identity_and_topology() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        model.register_thread_group(42, "i1").await.unwrap();
        model.start_thread_group(42, "i1", 9_001).await.unwrap();
        model.register_thread(42, 11, "i1").await.unwrap();
        model.register_thread_group(42, "i2").await.unwrap();
        model.start_thread_group(42, "i2", 9_002).await.unwrap();
        model.register_thread(42, 22, "i2").await.unwrap();
        let service = service(Arc::clone(&model));

        let listed = service
            .list_processes(ListProcessesRequest::default())
            .await
            .unwrap();
        assert_eq!(listed.processes.len(), 2);
        assert!(listed
            .processes
            .iter()
            .all(|process| process.process_id.starts_with("prc_")));
        assert_eq!(
            listed
                .processes
                .iter()
                .filter_map(|process| process.system_process_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["9001", "9002"]
        );
        let first = listed.processes[0].clone();
        let fetched = service
            .get_process(GetProcessRequest {
                process_id: first.process_id.clone(),
                ..GetProcessRequest::default()
            })
            .await
            .unwrap()
            .process
            .unwrap();
        assert_eq!(fetched, first);

        let target = session_target(first.session_id.clone());
        let threads = service
            .list_threads(ListThreadsRequest {
                target: Some(target),
                ..ListThreadsRequest::default()
            })
            .await
            .unwrap()
            .threads;
        assert_eq!(threads.len(), 2);
        assert!(threads.iter().all(|thread| thread.process_id.is_some()));
        let selected_thread = threads
            .iter()
            .find(|thread| thread.process_id.as_deref() == Some(&first.process_id))
            .unwrap();
        let filtered = service
            .list_processes(ListProcessesRequest {
                target: Some(PublicTarget {
                    selector: Some(target::Selector::Thread(ddb_api_types::v2::ThreadTarget {
                        thread_id: selected_thread.thread_id.clone(),
                    })),
                }),
                ..ListProcessesRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.processes, vec![first.clone()]);

        model.start_thread_group(42, "i1", 9_101).await.unwrap();
        let changed = service
            .get_process(GetProcessRequest {
                process_id: first.process_id,
                ..GetProcessRequest::default()
            })
            .await
            .unwrap()
            .process
            .unwrap();
        assert_eq!(changed.system_process_id.as_deref(), Some("9101"));
        assert!(changed.revision > first.revision);

        let snapshot = service
            .get_snapshot(GetSnapshotRequest::default())
            .await
            .unwrap()
            .snapshot
            .unwrap();
        assert_eq!(snapshot.processes.len(), 2);
        assert_eq!(snapshot.threads.len(), 2);
    }

    #[tokio::test]
    async fn signal_catalog_is_typed_paged_and_requires_one_resolved_session() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        let service = service_with_port(Arc::clone(&model), Arc::new(SignalCommandPort));
        let session_id = service
            .list_sessions(ListSessionsRequest::default())
            .await
            .unwrap()
            .sessions
            .remove(0)
            .session_id;

        let first = service
            .list_signals(ListSignalsRequest {
                target: Some(session_target(session_id)),
                page: Some(ddb_api_types::v2::PageRequest {
                    page_size: 1,
                    page_token: None,
                }),
                ..ListSignalsRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(first.signals.len(), 1);
        assert_eq!(first.signals[0].name, "SIGINT");
        assert!(first.signals[0].stop);
        assert!(!first.signals[0].pass);
        assert!(first.page.unwrap().next_page_token.is_some());

        model.register_session(43, "other", None).await;
        model.complete_session_activation(43, None).await;
        let error = service
            .list_signals(ListSignalsRequest {
                target: Some(PublicTarget {
                    selector: Some(target::Selector::Broadcast(
                        ddb_api_types::v2::BroadcastTarget::default(),
                    )),
                }),
                ..ListSignalsRequest::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), DdbErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn pending_command_projection_is_safe_typed_and_bounded_by_listing() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        let service = service(model);
        let projected = service
            .projection()
            .pending_command(&crate::api::read_model::ApiPendingCommandView {
                sid: 42,
                token: 99,
                operation_id: Some("op_public".to_string()),
                operation_kind: Some(OperationKind::RawCommand as u32),
                enqueued_at: SystemTime::UNIX_EPOCH,
                running: true,
            })
            .unwrap();
        assert!(projected.pending_command_id.starts_with("cmd_"));
        assert_ne!(projected.pending_command_id, "cmd_42:99");
        assert_eq!(projected.operation_id.as_deref(), Some("op_public"));
        assert_eq!(projected.kind, OperationKind::RawCommand as i32);
        assert!(projected.running);
        assert_eq!(projected.enqueued_at.unwrap().seconds, 0);

        let listed = service
            .list_pending_commands(ListPendingCommandsRequest::default())
            .await
            .unwrap();
        assert!(listed.pending_commands.is_empty());
        assert!(listed.page.unwrap().next_page_token.is_none());
    }

    #[tokio::test]
    async fn snapshot_exposes_cursor_sections_and_backend_neutral_resources() {
        let model = RuntimeModel::new();
        model.register_session(7, "worker", None).await;
        model.add_breakpoint(BkptLoc::new("src/main.rs", 12));
        let service = service(model);

        let response = service
            .get_snapshot(GetSnapshotRequest::default())
            .await
            .unwrap();
        let snapshot = response.snapshot.unwrap();
        assert_eq!(snapshot.server_instance_id, service.server_instance_id());
        assert!(snapshot.state_event_cursor.is_some());
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.breakpoints.len(), 1);
        let capabilities = snapshot
            .capabilities
            .expect("capabilities should be present");
        assert_eq!(
            capabilities.authentication_mode,
            "none-insecure-development"
        );
        assert!(capabilities
            .supported_operations
            .contains(&(OperationKind::Execute as i32)));
        assert!(capabilities
            .execution_actions
            .contains(&(ExecutionAction::Next as i32)));
        let next = capabilities
            .execution_action_capabilities
            .iter()
            .find(|capability| capability.action == ExecutionAction::Next as i32)
            .expect("next target scopes should be discoverable");
        assert_eq!(
            next.scopes,
            vec![ddb_api_types::v2::ExecutionScopeKind::Thread as i32]
        );
        let continue_scopes = &capabilities
            .execution_action_capabilities
            .iter()
            .find(|capability| capability.action == ExecutionAction::Continue as i32)
            .expect("continue target scopes should be discoverable")
            .scopes;
        assert!(continue_scopes.contains(&(ddb_api_types::v2::ExecutionScopeKind::Group as i32)));
        assert!(continue_scopes.contains(&(ddb_api_types::v2::ExecutionScopeKind::Fanout as i32)));

        assert!(!capabilities
            .ddb_features
            .contains(&"context_restore".to_string()));
        assert_eq!(
            capabilities.backends[0].capability_namespace.as_deref(),
            Some("ddb.backend.mock")
        );
        assert!(snapshot
            .included_sections
            .contains(&(SnapshotSection::Selection as i32)));
    }

    #[tokio::test]
    async fn target_scoped_snapshot_contains_only_resources_affecting_that_target() {
        let model = RuntimeModel::new();
        for (sid, pid, thread) in [(42, 4_200, 11), (43, 4_300, 22)] {
            model.register_session(sid, "worker", None).await;
            model.complete_session_activation(sid, None).await;
            model.register_thread_group(sid, "i1").await.unwrap();
            model.start_thread_group(sid, "i1", pid).await.unwrap();
            model.register_thread(sid, thread, "i1").await.unwrap();
            model
                .update_thread_statuses(sid, &[thread], ThreadStatus::STOPPED)
                .await
                .unwrap();
        }
        model
            .insert_breakpoint(
                BkptLoc::new("src/scoped.rs", 9),
                BreakpointProperties::default(),
                vec![SubBkptSpec::Session {
                    sid: 42,
                    local_id: 1,
                }],
            )
            .unwrap();
        let service = service(model);
        let session_id = service.ids.encode(ResourceIdKind::Session, 42).unwrap();
        let requested_target = session_target(session_id.clone());
        let snapshot = service
            .get_snapshot(GetSnapshotRequest {
                target: Some(requested_target.clone()),
                sections: vec![
                    SnapshotSection::Topology as i32,
                    SnapshotSection::Execution as i32,
                    SnapshotSection::Breakpoints as i32,
                ],
                ..GetSnapshotRequest::default()
            })
            .await
            .unwrap()
            .snapshot
            .unwrap();

        assert!(snapshot.state_event_cursor.is_some());
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].session_id, session_id);
        assert_eq!(snapshot.processes.len(), 1);
        assert_eq!(
            snapshot.processes[0].system_process_id.as_deref(),
            Some("4200")
        );
        assert_eq!(snapshot.threads.len(), 1);
        assert_eq!(snapshot.threads[0].session_id, session_id);
        assert_eq!(snapshot.execution_states.len(), 1);
        assert_eq!(
            snapshot.execution_states[0].target.as_ref(),
            Some(&requested_target)
        );
        assert_eq!(snapshot.breakpoints.len(), 1);
        assert_eq!(
            snapshot.breakpoints[0]
                .spec
                .as_ref()
                .and_then(|spec| spec.location.as_ref()),
            Some(&ddb_api_types::v2::breakpoint_spec::Location::Source(
                ddb_api_types::v2::SourceBreakpointLocation {
                    source: "src/scoped.rs".to_string(),
                    line: 9,
                    column: 0,
                }
            ))
        );
    }

    #[tokio::test]
    async fn mutation_admission_is_idempotent_principal_scoped_and_event_ordered() {
        let model = RuntimeModel::new();
        model.register_session(42, "worker", None).await;
        model.complete_session_activation(42, None).await;
        model.register_session(43, "other-worker", None).await;
        model.complete_session_activation(43, None).await;
        let port = Arc::new(RecordingCommandPort::default());
        let service = service_with_port(model, port.clone());
        let target = session_target(public_session_id(&service).await);
        let other_target = session_target(service.ids.encode(ResourceIdKind::Session, 43).unwrap());
        let principal = PrincipalContext::new("principal-a").unwrap();
        let request = execute_request("same-key", target.clone(), ExecutionAction::Continue);
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        let admitted = service
            .execute(&principal, request.clone())
            .await
            .expect("first mutation should be admitted")
            .operation
            .expect("admission should include an operation");
        let operation_id = admitted.operation_id.clone();
        let transitions = [
            next_operation_event(&mut events, &operation_id).await,
            next_operation_event(&mut events, &operation_id).await,
            next_operation_event(&mut events, &operation_id).await,
        ];
        assert_eq!(
            transitions
                .iter()
                .map(|operation| OperationState::try_from(operation.state).unwrap())
                .collect::<Vec<_>>(),
            vec![
                OperationState::Accepted,
                OperationState::Running,
                OperationState::Completed
            ]
        );
        assert_eq!(
            transitions
                .iter()
                .map(|operation| operation.revision)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);

        let replay = service
            .execute(&principal, request.clone())
            .await
            .expect("idempotent retry should succeed")
            .operation
            .unwrap();
        assert_eq!(replay.operation_id, operation_id);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);

        let conflict = service
            .execute(
                &principal,
                ExecuteRequest {
                    action: ExecutionAction::Interrupt as i32,
                    ..request.clone()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code(), DdbErrorCode::Conflict);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);

        let other_principal = PrincipalContext::new("principal-b").unwrap();
        let second = service
            .execute(&other_principal, request)
            .await
            .expect("a different principal owns a separate idempotency scope")
            .operation
            .unwrap();
        assert_ne!(second.operation_id, operation_id);
        let _ = next_operation_event(&mut events, &second.operation_id).await;
        let _ = next_operation_event(&mut events, &second.operation_id).await;
        let _ = next_operation_event(&mut events, &second.operation_id).await;
        assert_eq!(port.calls.load(Ordering::SeqCst), 2);

        let scoped = service
            .list_operations(ListOperationsRequest {
                target: Some(target.clone()),
                ..ListOperationsRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(scoped.operations.len(), 2);
        let unrelated = service
            .list_operations(ListOperationsRequest {
                target: Some(other_target),
                ..ListOperationsRequest::default()
            })
            .await
            .unwrap();
        assert!(unrelated.operations.is_empty());

        let duplicate_target = PublicTarget {
            selector: Some(target::Selector::Multiple(MultipleTarget {
                targets: vec![target.clone(), target],
            })),
        };
        let duplicate = service
            .execute(
                &principal,
                execute_request(
                    "duplicate-target",
                    duplicate_target,
                    ExecutionAction::Continue,
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(duplicate.code(), DdbErrorCode::InvalidArgument);
        assert_eq!(port.calls.load(Ordering::SeqCst), 2);

        let expired = service
            .execute(
                &principal,
                ExecuteRequest {
                    context: Some(RequestContext {
                        idempotency_key: Some("expired".to_string()),
                        deadline: Some(ddb_api_types::wkt::Timestamp {
                            seconds: 0,
                            nanos: 0,
                        }),
                        ..RequestContext::default()
                    }),
                    target: Some(session_target(public_session_id(&service).await)),
                    action: ExecutionAction::Next as i32,
                    ..ExecuteRequest::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(expired.code(), DdbErrorCode::DeadlineExceeded);
        assert_eq!(port.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn backend_failures_are_terminal_and_do_not_expose_internal_details() {
        let model = RuntimeModel::new();
        model.register_session(7, "worker", None).await;
        model.complete_session_activation(7, None).await;
        let port = Arc::new(RecordingCommandPort::default());
        port.fail.store(true, Ordering::SeqCst);
        let service = service_with_port(model, port);
        let principal = PrincipalContext::new("principal-a").unwrap();
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        let admitted = service
            .execute(
                &principal,
                execute_request(
                    "failing-command",
                    session_target(public_session_id(&service).await),
                    ExecutionAction::Continue,
                ),
            )
            .await
            .unwrap()
            .operation
            .unwrap();
        let _accepted = next_operation_event(&mut events, &admitted.operation_id).await;
        let _running = next_operation_event(&mut events, &admitted.operation_id).await;
        let failed = next_operation_event(&mut events, &admitted.operation_id).await;
        assert_eq!(
            OperationState::try_from(failed.state).unwrap(),
            OperationState::Failed
        );
        let error = failed
            .error
            .expect("failed operation should retain an error");
        assert_eq!(error.code, DdbErrorCode::BackendFailed as i32);
        assert_eq!(error.message, "debugger command failed");
        assert!(!error.message.contains("sensitive backend"));
    }

    #[tokio::test]
    async fn partial_fanout_retains_exact_outcomes_and_a_safe_partial_result() {
        let model = RuntimeModel::new();
        model.register_session(7, "successful", None).await;
        model.complete_session_activation(7, None).await;
        model.register_session(8, "failed", None).await;
        model.complete_session_activation(8, None).await;
        let report = CommandFanoutReport::new(
            None,
            vec![ParsedSessionResponse::new(7, "running".to_string(), None)],
            vec![SessionCommandFailure::new(
                8,
                SessionCommandFailureKind::DebuggerRejected,
            )],
        );
        let service = service_with_port(model, Arc::new(PartialCommandPort { report }));
        let successful_id = service.ids.encode(ResourceIdKind::Session, 7).unwrap();
        let failed_id = service.ids.encode(ResourceIdKind::Session, 8).unwrap();
        let target = ddb_api_types::v2::Target {
            selector: Some(target::Selector::SessionSet(
                ddb_api_types::v2::SessionSetTarget {
                    session_ids: vec![successful_id.clone(), failed_id.clone()],
                },
            )),
        };
        let principal = PrincipalContext::new("principal-a").unwrap();
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();

        let admitted = service
            .execute(
                &principal,
                execute_request("partial-command", target, ExecutionAction::Continue),
            )
            .await
            .unwrap()
            .operation
            .unwrap();
        let _accepted = next_operation_event(&mut events, &admitted.operation_id).await;
        let _running = next_operation_event(&mut events, &admitted.operation_id).await;
        let failed = next_operation_event(&mut events, &admitted.operation_id).await;

        assert_eq!(
            OperationState::try_from(failed.state).unwrap(),
            OperationState::Failed
        );
        let error = failed.error.as_ref().expect("aggregate error is required");
        assert_eq!(error.code, DdbErrorCode::PartialFailure as i32);
        assert_eq!(error.target_failures.len(), 1);
        assert_eq!(
            error.target_failures[0]
                .target
                .as_ref()
                .and_then(|target| target.selector.as_ref()),
            Some(&target::Selector::Session(
                ddb_api_types::v2::SessionTarget {
                    session_id: failed_id.clone(),
                }
            ))
        );
        assert_eq!(
            error.target_failures[0]
                .error
                .as_ref()
                .map(|error| error.code),
            Some(DdbErrorCode::BackendFailed as i32)
        );
        assert!(matches!(
            failed
                .result
                .as_ref()
                .and_then(|result| result.value.as_ref()),
            Some(ddb_api_types::v2::operation_result::Value::NoContent(_))
        ));

        assert_eq!(failed.target_outcomes.len(), 2);
        let successful = failed
            .target_outcomes
            .iter()
            .find(|outcome| {
                outcome
                    .target
                    .as_ref()
                    .and_then(|target| target.selector.as_ref())
                    == Some(&target::Selector::Session(
                        ddb_api_types::v2::SessionTarget {
                            session_id: successful_id.clone(),
                        },
                    ))
            })
            .expect("successful target outcome should be retained");
        assert!(successful.succeeded);
        assert!(successful.error.is_none());
        let failed_target = failed
            .target_outcomes
            .iter()
            .find(|outcome| {
                outcome
                    .target
                    .as_ref()
                    .and_then(|target| target.selector.as_ref())
                    == Some(&target::Selector::Session(
                        ddb_api_types::v2::SessionTarget {
                            session_id: failed_id.clone(),
                        },
                    ))
            })
            .expect("failed target outcome should be retained");
        assert!(!failed_target.succeeded);
        assert_eq!(
            failed_target.error.as_ref().map(|error| error.code),
            Some(DdbErrorCode::BackendFailed as i32)
        );
    }

    #[tokio::test]
    async fn sample_extension_is_discovered_scoped_idempotent_and_completed_as_an_operation() {
        let model = RuntimeModel::new();
        model.register_session(7, "worker", None).await;
        model.complete_session_activation(7, None).await;
        let service = service_with_plugin(model, Arc::new(SampleExtensionPlugin));

        let capabilities = service.capabilities().unwrap();
        assert_eq!(capabilities.extensions.len(), 1);
        assert_eq!(capabilities.extensions[0].extension_id, EXTENSION_ID);
        assert!(capabilities
            .supported_operations
            .contains(&(OperationKind::ExtensionAction as i32)));

        let target = session_target(service.ids.encode(ResourceIdKind::Session, 7).unwrap());
        let request = InvokeExtensionActionRequest {
            context: Some(RequestContext {
                idempotency_key: Some("sample-move-alpha".to_string()),
                ..RequestContext::default()
            }),
            extension_id: EXTENSION_ID.to_string(),
            action_id: MOVE_ACTION_ID.to_string(),
            payload: Some(move_worker_payload("alpha", "session-9")),
            target: Some(target),
            preconditions: None,
        };

        let read_principal = PrincipalContext::with_scope("reader", PermissionScope::Read).unwrap();
        let denied = service
            .invoke_extension_action(&read_principal, request.clone())
            .await
            .unwrap_err();
        assert_eq!(denied.code(), DdbErrorCode::PermissionDenied);

        let control_principal =
            PrincipalContext::with_scope("controller", PermissionScope::Control).unwrap();
        let mut events = service
            .subscribe_state_events(SubscribeStateEventsRequest::default())
            .unwrap();
        let admitted = service
            .invoke_extension_action(&control_principal, request.clone())
            .await
            .unwrap()
            .operation
            .unwrap();
        let transitions = [
            next_operation_event(&mut events, &admitted.operation_id).await,
            next_operation_event(&mut events, &admitted.operation_id).await,
            next_operation_event(&mut events, &admitted.operation_id).await,
        ];
        assert_eq!(
            transitions
                .iter()
                .map(|operation| OperationState::try_from(operation.state).unwrap())
                .collect::<Vec<_>>(),
            vec![
                OperationState::Accepted,
                OperationState::Running,
                OperationState::Completed
            ]
        );
        let completed = transitions.last().unwrap();
        let result_payload = match completed
            .result
            .as_ref()
            .and_then(|result| result.value.as_ref())
        {
            Some(operation_result::Value::ExtensionAction(result)) => {
                result.payload.as_ref().unwrap()
            }
            other => panic!("unexpected extension action result: {other:?}"),
        };
        assert_eq!(result_payload.extension_id, EXTENSION_ID);
        assert_eq!(completed.target_outcomes.len(), 1);
        assert!(completed.target_outcomes[0].succeeded);
        assert_eq!(
            completed.target_outcomes[0]
                .target
                .as_ref()
                .and_then(|target| target.selector.as_ref()),
            request
                .target
                .as_ref()
                .and_then(|target| target.selector.as_ref())
        );

        let schema = service
            .get_extension_schema(GetExtensionSchemaRequest {
                context: None,
                extension_id: EXTENSION_ID.to_string(),
                schema_uri: ROOT_SCHEMA_URI.to_string(),
            })
            .unwrap()
            .schema
            .unwrap();
        assert_eq!(schema.extension_id, EXTENSION_ID);
        assert_eq!(schema.schema_uri, ROOT_SCHEMA_URI);
        assert_eq!(schema.content_sha256.len(), 64);
        assert!(std::str::from_utf8(&schema.content)
            .unwrap()
            .contains("presentations"));

        let replay = service
            .invoke_extension_action(&control_principal, request)
            .await
            .unwrap()
            .operation
            .unwrap();
        assert_eq!(replay.operation_id, admitted.operation_id);
        assert_eq!(
            OperationState::try_from(replay.state).unwrap(),
            OperationState::Completed
        );

        let states = service
            .list_extension_states(ListExtensionStatesRequest::default())
            .unwrap();
        let state_json = match states.extension_states[0].payloads[0]
            .payload
            .as_ref()
            .unwrap()
        {
            extension_payload::Payload::PayloadJson(json) => json,
            extension_payload::Payload::PayloadBytes(_) => panic!("expected JSON state"),
        };
        assert!(state_json.contains("session-9"));
    }
}
