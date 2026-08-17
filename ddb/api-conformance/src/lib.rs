//! Black-box DDB API v2 conformance checks.
//!
//! The runner imports only the published SDK. It deliberately has no access to
//! DDB core state, test-only routes, or backend-local identifiers.

use std::{
    collections::HashSet,
    fmt,
    future::Future,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use ddb_api_client::{v2, ClientConfig, ClientError, DdbClient};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use url::Url;

const RUNNER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Conformance scope selected by the operator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceProfile {
    /// Safe discovery, topology, inspection, and stream-connect checks.
    ReadOnly,
    /// Full deterministic Mock workflow, including idempotent controls.
    Mock,
}

impl fmt::Display for ConformanceProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReadOnly => "read_only",
            Self::Mock => "mock",
        })
    }
}

/// Bounded runtime policy for one conformance run.
#[derive(Clone)]
pub struct ConformanceOptions {
    pub endpoint: String,
    pub bearer_token: Option<String>,
    pub profile: ConformanceProfile,
    pub max_collection_items: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub stream_timeout: Duration,
}

impl Default for ConformanceOptions {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:5000".to_string(),
            bearer_token: None,
            profile: ConformanceProfile::ReadOnly,
            max_collection_items: 10_000,
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(10),
            stream_timeout: Duration::from_secs(5),
        }
    }
}

impl fmt::Debug for ConformanceOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConformanceOptions")
            .field("endpoint", &safe_endpoint(&self.endpoint))
            .field("authenticated", &self.bearer_token.is_some())
            .field("profile", &self.profile)
            .field("max_collection_items", &self.max_collection_items)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("stream_timeout", &self.stream_timeout)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub duration_millis: u64,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ConformanceSummary {
    pub sessions: usize,
    pub groups: usize,
    pub processes: usize,
    pub threads: usize,
    pub breakpoints: usize,
    pub pending_commands: usize,
    pub operations: usize,
    pub extension_states: usize,
    pub frames: usize,
    pub scopes: usize,
    pub variables: usize,
    pub registers: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConformanceReport {
    pub runner_version: String,
    pub profile: ConformanceProfile,
    pub endpoint: String,
    pub started_unix_millis: u64,
    pub completed_unix_millis: u64,
    pub server_instance_id: Option<String>,
    pub server_version: Option<String>,
    pub api_version: Option<String>,
    pub schema_version: Option<String>,
    pub summary: ConformanceSummary,
    pub checks: Vec<CheckResult>,
}

impl ConformanceReport {
    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status != CheckStatus::Failed)
    }

    pub fn passed_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Passed)
            .count()
    }

    pub fn failed_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Failed)
            .count()
    }

    pub fn skipped_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Skipped)
            .count()
    }
}

/// Runs the selected profile against a server using only public API calls.
pub async fn run(options: ConformanceOptions) -> Result<ConformanceReport> {
    validate_options(&options)?;
    let mut report = ConformanceReport {
        runner_version: RUNNER_VERSION.to_string(),
        profile: options.profile,
        endpoint: safe_endpoint(&options.endpoint)?,
        started_unix_millis: unix_millis(),
        completed_unix_millis: 0,
        server_instance_id: None,
        server_version: None,
        api_version: None,
        schema_version: None,
        summary: ConformanceSummary::default(),
        checks: Vec::new(),
    };

    let mut config = ClientConfig::new(&options.endpoint);
    config.connect_timeout = options.connect_timeout;
    config.request_timeout = options.request_timeout;
    if let Some(token) = options.bearer_token {
        config = config.with_bearer_token(token);
    }
    let client = match DdbClient::new(config) {
        Ok(client) => client,
        Err(error) => {
            fail(&mut report, "client.configuration", error.to_string(), 0);
            return Ok(finish(report));
        }
    };

    let Some((server, capabilities)) =
        record(&mut report, "discovery.handshake", client.handshake()).await
    else {
        return Ok(finish(report));
    };
    report.server_instance_id = Some(server.server_instance_id.clone());
    report.server_version = Some(server.version.clone());
    report.api_version = Some(capabilities.api_version.clone());
    report.schema_version = Some(capabilities.schema_version.clone());
    record_sync(
        &mut report,
        "discovery.contract",
        validate_discovery(&server, &capabilities),
    );

    record(&mut report, "health.liveness", async {
        let response = client.get_health(v2::GetHealthRequest::default()).await?;
        let health = response.health.context("GetHealth omitted health report")?;
        if health.server_instance_id != server.server_instance_id {
            bail!("health report belongs to a different server instance");
        }
        if health.status != v2::HealthStatus::Up as i32 {
            bail!("health status is not UP");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    record(&mut report, "health.readiness", async {
        let response = client
            .get_readiness(v2::GetReadinessRequest::default())
            .await?;
        let readiness = response
            .readiness
            .context("GetReadiness omitted readiness report")?;
        if readiness.server_instance_id != server.server_instance_id {
            bail!("readiness report belongs to a different server instance");
        }
        if readiness.status != v2::HealthStatus::Up as i32 {
            bail!("readiness status is not UP");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;

    let snapshot_request = v2::GetSnapshotRequest {
        sections: all_snapshot_sections(),
        ..Default::default()
    };
    let snapshot = record(&mut report, "state.snapshot", async {
        client
            .get_snapshot(snapshot_request)
            .await?
            .snapshot
            .context("GetSnapshot omitted snapshot")
    })
    .await;
    if let Some(snapshot) = snapshot.as_ref() {
        record_sync(
            &mut report,
            "state.snapshot_integrity",
            validate_snapshot(snapshot, &server.server_instance_id),
        );
    }

    if let Some(counts) = record(
        &mut report,
        "collections.pagination",
        collect_and_validate(&client, options.max_collection_items),
    )
    .await
    {
        report.summary = counts;
    }

    if let Some(snapshot) = snapshot.as_ref() {
        let cursor = snapshot.state_event_cursor.clone();
        record(&mut report, "streams.state_connect", async {
            let stream = timeout(
                options.stream_timeout,
                client.subscribe_state_events(v2::SubscribeStateEventsRequest {
                    context: None,
                    after_cursor: cursor,
                    filter: None,
                }),
            )
            .await
            .context("state stream did not connect before the timeout")??;
            drop(stream);
            Ok::<_, anyhow::Error>(())
        })
        .await;
    } else {
        skip(
            &mut report,
            "streams.state_connect",
            "snapshot was unavailable",
        );
    }
    record(&mut report, "streams.output_connect", async {
        let stream = timeout(
            options.stream_timeout,
            client.subscribe_output(v2::SubscribeOutputRequest::default()),
        )
        .await
        .context("output stream did not connect before the timeout")??;
        drop(stream);
        Ok::<_, anyhow::Error>(())
    })
    .await;

    let stopped_thread = snapshot.as_ref().and_then(|snapshot| {
        snapshot
            .threads
            .iter()
            .find(|thread| thread.state == v2::ThreadState::Stopped as i32)
            .cloned()
    });
    if let Some(thread) = stopped_thread.as_ref() {
        if let Some(inspection) = record(
            &mut report,
            "inspection.stopped_thread",
            inspect_stopped_thread(&client, thread, options.max_collection_items, &capabilities),
        )
        .await
        {
            report.summary.frames = inspection.frames;
            report.summary.scopes = inspection.scopes;
            report.summary.variables = inspection.variables;
            report.summary.registers = inspection.registers;
        }
    } else {
        skip(
            &mut report,
            "inspection.stopped_thread",
            "no stopped thread was present in the requested snapshot",
        );
    }

    if options.profile == ConformanceProfile::Mock {
        match stopped_thread {
            Some(thread) => {
                record(
                    &mut report,
                    "mock.control_workflow",
                    mock_control_workflow(&client, &thread, &capabilities, options.stream_timeout),
                )
                .await;
            }
            None => skip(
                &mut report,
                "mock.control_workflow",
                "the Mock profile requires one stopped thread",
            ),
        }
    }

    Ok(finish(report))
}

async fn record<T, E>(
    report: &mut ConformanceReport,
    name: &str,
    future: impl Future<Output = std::result::Result<T, E>>,
) -> Option<T>
where
    E: fmt::Display,
{
    let started = Instant::now();
    match future.await {
        Ok(value) => {
            pass(report, name, "ok", elapsed_millis(started));
            Some(value)
        }
        Err(error) => {
            fail(report, name, format!("{error:#}"), elapsed_millis(started));
            None
        }
    }
}

fn record_sync<T>(report: &mut ConformanceReport, name: &str, result: Result<T>) -> Option<T> {
    match result {
        Ok(value) => {
            pass(report, name, "ok", 0);
            Some(value)
        }
        Err(error) => {
            fail(report, name, error.to_string(), 0);
            None
        }
    }
}

fn pass(report: &mut ConformanceReport, name: &str, detail: &str, duration_millis: u64) {
    report.checks.push(CheckResult {
        name: name.to_string(),
        status: CheckStatus::Passed,
        duration_millis,
        detail: detail.to_string(),
    });
}

fn fail(report: &mut ConformanceReport, name: &str, detail: String, duration_millis: u64) {
    report.checks.push(CheckResult {
        name: name.to_string(),
        status: CheckStatus::Failed,
        duration_millis,
        detail,
    });
}

fn skip(report: &mut ConformanceReport, name: &str, detail: &str) {
    report.checks.push(CheckResult {
        name: name.to_string(),
        status: CheckStatus::Skipped,
        duration_millis: 0,
        detail: detail.to_string(),
    });
}

fn finish(mut report: ConformanceReport) -> ConformanceReport {
    report.completed_unix_millis = unix_millis();
    report
}

fn validate_options(options: &ConformanceOptions) -> Result<()> {
    if options.max_collection_items == 0 {
        bail!("max_collection_items must be greater than zero");
    }
    if options.connect_timeout.is_zero()
        || options.request_timeout.is_zero()
        || options.stream_timeout.is_zero()
    {
        bail!("all conformance timeouts must be greater than zero");
    }
    safe_endpoint(&options.endpoint)?;
    Ok(())
}

fn validate_discovery(server: &v2::ServerInfo, capabilities: &v2::Capabilities) -> Result<()> {
    if server.name != "ddb" {
        bail!("GetServerInfo name is not ddb");
    }
    if server.server_instance_id.is_empty() {
        bail!("GetServerInfo omitted server_instance_id");
    }
    if !server.api_versions.iter().any(|version| version == "v2") {
        bail!("server does not advertise API v2");
    }
    if capabilities.api_version != "v2" || capabilities.schema_version.is_empty() {
        bail!("capabilities omitted a supported v2 schema version");
    }
    if capabilities.server_instance_id != server.server_instance_id {
        bail!("capabilities and server info use different server instances");
    }
    if capabilities.capabilities_id.is_empty() || capabilities.revision == 0 {
        bail!("capabilities are not identified and revisioned");
    }
    if !capabilities
        .transports
        .iter()
        .any(|transport| transport.transport == v2::TransportKind::Http as i32)
    {
        bail!("mandatory HTTP transport is not advertised");
    }
    let limits = capabilities
        .limits
        .as_ref()
        .context("capabilities omitted effective limits")?;
    if limits.max_page_size == 0
        || limits.max_response_bytes == 0
        || limits.max_memory_read_bytes == 0
        || limits.max_source_lines == 0
        || limits.state_subscriber_queue == 0
        || limits.output_subscriber_queue == 0
        || limits.max_subscribers == 0
        || limits.max_operation_records == 0
    {
        bail!("one or more mandatory advertised limits are zero");
    }
    unique_strings(
        capabilities
            .extensions
            .iter()
            .map(|extension| extension.extension_id.as_str()),
        "extension IDs",
    )?;
    for extension in &capabilities.extensions {
        if extension.extension_id.is_empty()
            || extension.version.is_empty()
            || extension.schema_uri.is_empty()
            || extension.schema_hash.is_empty()
        {
            bail!("extension descriptor is incomplete");
        }
        unique_strings(
            extension
                .presentations
                .iter()
                .map(|presentation| presentation.id.as_str()),
            "extension presentation IDs",
        )?;
    }
    Ok(())
}

fn validate_snapshot(snapshot: &v2::Snapshot, server_instance_id: &str) -> Result<()> {
    if snapshot.server_instance_id != server_instance_id {
        bail!("snapshot belongs to a different server instance");
    }
    let cursor = snapshot
        .state_event_cursor
        .as_ref()
        .context("snapshot omitted state_event_cursor")?;
    if cursor.server_instance_id != server_instance_id {
        bail!("snapshot cursor belongs to a different server instance");
    }
    let included = snapshot
        .included_sections
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    for section in all_snapshot_sections() {
        if !included.contains(&section) {
            bail!("snapshot omitted requested section {section}");
        }
    }

    unique_strings(
        snapshot
            .sessions
            .iter()
            .map(|session| session.session_id.as_str()),
        "session IDs",
    )?;
    unique_strings(
        snapshot.groups.iter().map(|group| group.group_id.as_str()),
        "group IDs",
    )?;
    unique_strings(
        snapshot
            .processes
            .iter()
            .map(|process| process.process_id.as_str()),
        "process IDs",
    )?;
    unique_strings(
        snapshot
            .threads
            .iter()
            .map(|thread| thread.thread_id.as_str()),
        "thread IDs",
    )?;
    unique_strings(
        snapshot
            .breakpoints
            .iter()
            .map(|breakpoint| breakpoint.breakpoint_id.as_str()),
        "breakpoint IDs",
    )?;
    unique_strings(
        snapshot
            .operations
            .iter()
            .map(|operation| operation.operation_id.as_str()),
        "operation IDs",
    )?;

    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect::<HashSet<_>>();
    let groups = snapshot
        .groups
        .iter()
        .map(|group| group.group_id.as_str())
        .collect::<HashSet<_>>();
    let processes = snapshot
        .processes
        .iter()
        .map(|process| process.process_id.as_str())
        .collect::<HashSet<_>>();
    let threads = snapshot
        .threads
        .iter()
        .map(|thread| thread.thread_id.as_str())
        .collect::<HashSet<_>>();

    for group in &snapshot.groups {
        if group
            .session_ids
            .iter()
            .any(|session_id| !sessions.contains(session_id.as_str()))
        {
            bail!("group references a session outside the snapshot");
        }
    }
    for process in &snapshot.processes {
        if !sessions.contains(process.session_id.as_str())
            || process
                .group_id
                .as_deref()
                .is_some_and(|group_id| !groups.contains(group_id))
        {
            bail!("process has an invalid topology reference");
        }
    }
    for thread in &snapshot.threads {
        if !sessions.contains(thread.session_id.as_str())
            || thread
                .process_id
                .as_deref()
                .is_some_and(|process_id| !processes.contains(process_id))
            || thread
                .group_id
                .as_deref()
                .is_some_and(|group_id| !groups.contains(group_id))
        {
            bail!("thread has an invalid topology reference");
        }
    }
    if let Some(selection) = snapshot.selection.as_ref() {
        if selection
            .session_id
            .as_deref()
            .is_some_and(|session_id| !sessions.contains(session_id))
            || selection
                .group_id
                .as_deref()
                .is_some_and(|group_id| !groups.contains(group_id))
            || selection
                .thread_id
                .as_deref()
                .is_some_and(|thread_id| !threads.contains(thread_id))
        {
            bail!("selection references a resource outside the snapshot");
        }
    }
    for command in &snapshot.pending_commands {
        if !sessions.contains(command.session_id.as_str()) {
            bail!("pending command references a session outside the snapshot");
        }
    }
    if snapshot
        .capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.server_instance_id != server_instance_id)
    {
        bail!("snapshot capability document belongs to another server instance");
    }
    Ok(())
}

async fn collect_and_validate(client: &DdbClient, max_items: usize) -> Result<ConformanceSummary> {
    let sessions = client
        .collect_sessions(
            v2::ListSessionsRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListSessions pagination failed")?;
    let groups = client
        .collect_groups(
            v2::ListGroupsRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListGroups pagination failed")?;
    let processes = client
        .collect_processes(
            v2::ListProcessesRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListProcesses pagination failed")?;
    let mut threads = Vec::new();
    for session in &sessions {
        // ListThreads intentionally requires a canonical target. Traverse
        // each visible session so this remains valid on servers with no
        // process-wide "all threads" selector while retaining a global bound.
        let remaining = max_items.saturating_sub(threads.len()).max(1);
        let mut session_threads = client
            .collect_threads(
                v2::ListThreadsRequest {
                    target: Some(session_target(&session.session_id)),
                    page: page_one(),
                    ..Default::default()
                },
                remaining,
            )
            .await
            .with_context(|| {
                format!(
                    "ListThreads pagination failed for session {}",
                    session.session_id
                )
            })?;
        threads.append(&mut session_threads);
        if threads.len() > max_items {
            bail!("paginated threads exceeded the configured item bound");
        }
    }
    let breakpoints = client
        .collect_breakpoints(
            v2::ListBreakpointsRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListBreakpoints pagination failed")?;
    let pending_commands = client
        .collect_pending_commands(
            v2::ListPendingCommandsRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListPendingCommands pagination failed")?;
    let operations = client
        .collect_operations(
            v2::ListOperationsRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListOperations pagination failed")?;
    let extension_states = client
        .collect_extension_states(
            v2::ListExtensionStatesRequest {
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
        .context("ListExtensionStates pagination failed")?;

    unique_strings(
        sessions.iter().map(|resource| resource.session_id.as_str()),
        "paginated sessions",
    )?;
    unique_strings(
        groups.iter().map(|resource| resource.group_id.as_str()),
        "paginated groups",
    )?;
    unique_strings(
        processes
            .iter()
            .map(|resource| resource.process_id.as_str()),
        "paginated processes",
    )?;
    unique_strings(
        threads.iter().map(|resource| resource.thread_id.as_str()),
        "paginated threads",
    )?;
    unique_strings(
        breakpoints
            .iter()
            .map(|resource| resource.breakpoint_id.as_str()),
        "paginated breakpoints",
    )?;
    unique_strings(
        pending_commands
            .iter()
            .map(|resource| resource.pending_command_id.as_str()),
        "paginated pending commands",
    )?;
    unique_strings(
        operations
            .iter()
            .map(|resource| resource.operation_id.as_str()),
        "paginated operations",
    )?;
    unique_strings(
        extension_states
            .iter()
            .map(|resource| resource.extension_state_id.as_str()),
        "paginated extension states",
    )?;

    Ok(ConformanceSummary {
        sessions: sessions.len(),
        groups: groups.len(),
        processes: processes.len(),
        threads: threads.len(),
        breakpoints: breakpoints.len(),
        pending_commands: pending_commands.len(),
        operations: operations.len(),
        extension_states: extension_states.len(),
        ..Default::default()
    })
}

#[derive(Default)]
struct InspectionSummary {
    frames: usize,
    scopes: usize,
    variables: usize,
    registers: usize,
}

async fn inspect_stopped_thread(
    client: &DdbClient,
    thread: &v2::Thread,
    max_items: usize,
    capabilities: &v2::Capabilities,
) -> Result<InspectionSummary> {
    let frames = client
        .collect_frames(
            v2::ListFramesRequest {
                thread_id: thread.thread_id.clone(),
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await?;
    let frame = frames
        .first()
        .context("stopped thread returned no stack frames")?;
    if frame.thread_id != thread.thread_id || frame.frame_id.is_empty() {
        bail!("frame does not belong to the requested thread");
    }
    let scopes = client
        .collect_scopes(
            v2::ListScopesRequest {
                frame_id: frame.frame_id.clone(),
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await?;
    let mut variable_count = 0;
    for scope in &scopes {
        let variables = client
            .collect_variables(
                v2::ListVariablesRequest {
                    scope_id: scope.scope_id.clone(),
                    page: page_one(),
                    ..Default::default()
                },
                max_items.saturating_sub(variable_count),
            )
            .await?;
        variable_count = variable_count.saturating_add(variables.len());
        if variable_count > max_items {
            bail!("combined scope variables exceeded the client bound");
        }
    }

    let registers = match client
        .collect_registers(
            v2::ListRegistersRequest {
                frame_id: frame.frame_id.clone(),
                format: v2::RegisterFormat::Natural as i32,
                page: page_one(),
                ..Default::default()
            },
            max_items,
        )
        .await
    {
        Ok(registers) => registers.len(),
        Err(error) if is_unsupported(&error) => 0,
        Err(error) => return Err(error.into()),
    };

    if let Some(location) = frame
        .location
        .clone()
        .filter(|location| location.path.is_some() || location.source_reference.is_some())
    {
        let source = client
            .resolve_source(v2::ResolveSourceRequest {
                context: None,
                target: Some(thread_target(&thread.thread_id)),
                location: Some(location),
            })
            .await?
            .source
            .context("ResolveSource omitted source")?;
        let max_lines = capabilities
            .limits
            .as_ref()
            .map(|limits| limits.max_source_lines.min(64))
            .unwrap_or(64)
            .max(1);
        let content = client
            .read_source(v2::ReadSourceRequest {
                context: None,
                source_reference: source.source_reference,
                start_line: 1,
                max_lines,
            })
            .await?
            .source
            .context("ReadSource omitted source content")?;
        validate_source_content(&content)?;
    }

    Ok(InspectionSummary {
        frames: frames.len(),
        scopes: scopes.len(),
        variables: variable_count,
        registers,
    })
}

async fn mock_control_workflow(
    client: &DdbClient,
    thread: &v2::Thread,
    capabilities: &v2::Capabilities,
    stream_timeout: Duration,
) -> Result<()> {
    if !capabilities
        .backends
        .iter()
        .any(|backend| backend.kind == v2::BackendKind::Mock as i32)
    {
        bail!("Mock profile requires a server advertising the Mock backend");
    }
    let target = thread_target(&thread.thread_id);
    let frames = client
        .collect_frames(
            v2::ListFramesRequest {
                thread_id: thread.thread_id.clone(),
                page: None,
                ..Default::default()
            },
            128,
        )
        .await?;
    let frame = frames.first().context("Mock thread returned no frame")?;
    let before_line = frame.location.as_ref().map(|location| location.line);

    let idempotency_key = DdbClient::new_idempotency_key();
    let evaluation_request = v2::EvaluateRequest {
        context: Some(mutation_context(idempotency_key)),
        target: Some(target.clone()),
        expression: "counter".to_string(),
        frame_id: Some(frame.frame_id.clone()),
        evaluation_context: v2::EvaluationContext::Watch as i32,
        preconditions: None,
    };
    let first = client.evaluate(evaluation_request.clone()).await?;
    let duplicate = client.evaluate(evaluation_request).await?;
    let first = first
        .operation
        .context("Evaluate omitted admitted operation")?;
    let duplicate = duplicate
        .operation
        .context("duplicate Evaluate omitted admitted operation")?;
    if first.operation_id != duplicate.operation_id {
        bail!("repeated idempotency key admitted two evaluation operations");
    }
    let evaluation = terminal_operation(client, first.operation_id).await?;
    let evaluated = evaluation
        .result
        .as_ref()
        .and_then(|result| result.value.as_ref())
        .is_some_and(|value| {
            matches!(
                value,
                v2::operation_result::Value::Evaluation(result) if result.value == "42"
            )
        });
    if !evaluated {
        bail!("Mock evaluation did not return the expected typed value");
    }

    let memory = client
        .read_memory(v2::ReadMemoryRequest {
            context: None,
            target: Some(target.clone()),
            address: "0x1000".to_string(),
            byte_count: 8,
        })
        .await?
        .memory
        .context("ReadMemory omitted memory")?;
    if memory.address != "0x1000" || memory.data.len() != 8 {
        bail!("Mock memory response did not preserve address and byte count");
    }

    let mut output = timeout(
        stream_timeout,
        client.subscribe_output(v2::SubscribeOutputRequest::default()),
    )
    .await
    .context("Mock output stream did not connect")??;
    let raw = client
        .execute_raw_command(v2::ExecuteRawCommandRequest {
            context: Some(mutation_context(DdbClient::new_idempotency_key())),
            target: Some(target.clone()),
            dialect: v2::RawCommandDialect::GdbMi as i32,
            command: "-mock-stream-output".to_string(),
            preconditions: None,
        })
        .await?
        .operation
        .context("ExecuteRawCommand omitted admitted operation")?;
    terminal_operation(client, raw.operation_id).await?;
    timeout(stream_timeout, async {
        loop {
            let event = output
                .next()
                .await?
                .context("output stream ended before Mock output")?;
            if matches!(
                event.content.as_ref(),
                Some(v2::output_event::Content::Text(text)) if text.contains("mock console output")
            ) {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await
    .context("Mock output event did not arrive")??;

    let location = frame
        .location
        .as_ref()
        .and_then(|location| location.path.clone().map(|path| (path, location.line)))
        .context("Mock frame omitted a source location")?;
    let breakpoint_target = session_target(&thread.session_id);
    let created = client
        .create_breakpoint(v2::CreateBreakpointRequest {
            context: Some(mutation_context(DdbClient::new_idempotency_key())),
            target: Some(breakpoint_target.clone()),
            breakpoint: Some(v2::BreakpointSpec {
                location: Some(v2::breakpoint_spec::Location::Source(
                    v2::SourceBreakpointLocation {
                        source: location.0,
                        line: location.1,
                        column: 0,
                    },
                )),
                enabled: Some(false),
                temporary: true,
                ..Default::default()
            }),
            preconditions: None,
        })
        .await?
        .operation
        .context("CreateBreakpoint omitted admitted operation")?;
    let created = terminal_operation(client, created.operation_id).await?;
    let breakpoint = created
        .result
        .as_ref()
        .and_then(|result| result.value.as_ref())
        .and_then(|value| match value {
            v2::operation_result::Value::Breakpoint(breakpoint) => Some(breakpoint),
            _ => None,
        })
        .context("CreateBreakpoint omitted typed breakpoint result")?;
    if breakpoint
        .spec
        .as_ref()
        .is_none_or(|spec| spec.enabled != Some(false))
    {
        bail!("CreateBreakpoint did not preserve an atomically disabled breakpoint");
    }
    let deleted = client
        .delete_breakpoint(v2::DeleteBreakpointRequest {
            context: Some(mutation_context(DdbClient::new_idempotency_key())),
            breakpoint_id: breakpoint.breakpoint_id.clone(),
            target: Some(breakpoint_target),
            preconditions: None,
        })
        .await?
        .operation
        .context("DeleteBreakpoint omitted admitted operation")?;
    terminal_operation(client, deleted.operation_id).await?;

    if !capabilities
        .ddb_features
        .iter()
        .any(|feature| feature == "distributed_backtrace")
    {
        bail!("Mock server does not advertise distributed_backtrace");
    }
    let distributed = client
        .run_distributed_backtrace(v2::RunDistributedBacktraceRequest {
            context: Some(mutation_context(DdbClient::new_idempotency_key())),
            target: Some(target.clone()),
            max_frames: 32,
            preconditions: None,
        })
        .await?
        .operation
        .context("RunDistributedBacktrace omitted admitted operation")?;
    let distributed = terminal_operation(client, distributed.operation_id).await?;
    let backtrace = distributed
        .result
        .as_ref()
        .and_then(|result| result.value.as_ref())
        .and_then(|value| match value {
            v2::operation_result::Value::DistributedBacktrace(backtrace) => Some(backtrace),
            _ => None,
        })
        .context("RunDistributedBacktrace omitted its typed result")?;
    if backtrace.frames.is_empty() || backtrace.frames.len() > 32 {
        bail!("distributed backtrace returned an invalid bounded frame set");
    }
    unique_strings(
        backtrace.frames.iter().map(|frame| {
            frame
                .frame
                .as_ref()
                .map_or("", |frame| frame.frame_id.as_str())
        }),
        "distributed frame IDs",
    )?;

    let snapshot = client
        .get_snapshot(v2::GetSnapshotRequest {
            sections: vec![
                v2::SnapshotSection::Execution as i32,
                v2::SnapshotSection::PendingOperations as i32,
            ],
            ..Default::default()
        })
        .await?
        .snapshot
        .context("pre-step snapshot was omitted")?;
    let mut state = timeout(
        stream_timeout,
        client.subscribe_state_events(v2::SubscribeStateEventsRequest {
            context: None,
            after_cursor: snapshot.state_event_cursor,
            filter: None,
        }),
    )
    .await
    .context("Mock state stream did not connect")??;
    let next = client
        .execute(v2::ExecuteRequest {
            context: Some(mutation_context(DdbClient::new_idempotency_key())),
            target: Some(target),
            action: v2::ExecutionAction::Next as i32,
            ..Default::default()
        })
        .await?
        .operation
        .context("Execute omitted admitted operation")?;
    terminal_operation(client, next.operation_id.clone()).await?;
    timeout(stream_timeout, async {
        loop {
            let event = state
                .next()
                .await?
                .context("state stream ended before execution changed")?;
            if event.kind == v2::StateEventKind::ExecutionChanged as i32 {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await
    .context("Mock execution event did not arrive")??;
    let after_frames = client
        .collect_frames(
            v2::ListFramesRequest {
                thread_id: thread.thread_id.clone(),
                ..Default::default()
            },
            128,
        )
        .await?;
    let after_line = after_frames
        .first()
        .and_then(|frame| frame.location.as_ref())
        .map(|location| location.line);
    if before_line.is_some() && before_line == after_line {
        bail!("Step-over completed without moving the Mock execution line");
    }
    Ok(())
}

async fn terminal_operation(client: &DdbClient, operation_id: String) -> Result<v2::Operation> {
    let operation = client
        .wait_operation(
            operation_id,
            Duration::from_secs(5),
            Duration::from_millis(20),
        )
        .await?;
    if operation.state != v2::OperationState::Completed as i32 {
        bail!(
            "operation {} ended in state {}",
            operation.operation_id,
            operation.state
        );
    }
    Ok(operation)
}

fn validate_source_content(content: &v2::SourceContent) -> Result<()> {
    if content.start_line == 0 {
        bail!("source content start_line is not one-based");
    }
    let actual_lines = if content.line_count == 0 && content.content.is_empty() {
        0
    } else {
        content.content.split('\n').count()
    };
    if actual_lines != content.line_count as usize {
        bail!("source content line_count disagrees with newline-delimited content");
    }
    if content.source.is_none() {
        bail!("source content omitted source identity");
    }
    Ok(())
}

fn is_unsupported(error: &ClientError) -> bool {
    error
        .ddb_error()
        .is_some_and(|error| error.code == v2::DdbErrorCode::Unsupported as i32)
}

fn unique_strings<'a>(values: impl IntoIterator<Item = &'a str>, label: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for value in values {
        if value.is_empty() {
            bail!("{label} contain an empty identifier");
        }
        if !seen.insert(value) {
            bail!("{label} contain duplicate identifier {value:?}");
        }
    }
    Ok(())
}

fn all_snapshot_sections() -> Vec<i32> {
    vec![
        v2::SnapshotSection::Topology as i32,
        v2::SnapshotSection::Selection as i32,
        v2::SnapshotSection::Execution as i32,
        v2::SnapshotSection::Breakpoints as i32,
        v2::SnapshotSection::PendingOperations as i32,
        v2::SnapshotSection::Extensions as i32,
        v2::SnapshotSection::Capabilities as i32,
    ]
}

fn page_one() -> Option<v2::PageRequest> {
    Some(v2::PageRequest {
        page_size: 1,
        page_token: None,
    })
}

fn thread_target(thread_id: &str) -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Thread(v2::ThreadTarget {
            thread_id: thread_id.to_string(),
        })),
    }
}

fn session_target(session_id: &str) -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Session(v2::SessionTarget {
            session_id: session_id.to_string(),
        })),
    }
}

fn mutation_context(idempotency_key: String) -> v2::RequestContext {
    v2::RequestContext {
        idempotency_key: Some(idempotency_key),
        ..Default::default()
    }
}

fn safe_endpoint(endpoint: &str) -> Result<String> {
    let mut url = Url::parse(endpoint).context("invalid conformance endpoint")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("conformance endpoint scheme must be http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("supply credentials separately, not in the conformance endpoint");
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities() -> v2::Capabilities {
        v2::Capabilities {
            capabilities_id: "cap_opaque".to_string(),
            api_version: "v2".to_string(),
            schema_version: "2.0.0-test".to_string(),
            server_instance_id: "server".to_string(),
            transports: vec![v2::TransportEndpoint {
                transport: v2::TransportKind::Http as i32,
                uri: "http://127.0.0.1:5000".to_string(),
                encodings: vec![v2::WireEncoding::Protojson as i32],
                tls_required: false,
            }],
            limits: Some(v2::ApiLimits {
                max_page_size: 100,
                max_response_bytes: 1024,
                max_memory_read_bytes: 128,
                max_source_lines: 64,
                state_subscriber_queue: 8,
                output_subscriber_queue: 8,
                max_subscribers: 4,
                max_operation_records: 32,
                ..Default::default()
            }),
            revision: 1,
            ..Default::default()
        }
    }

    #[test]
    fn discovery_validation_requires_matching_revisioned_capabilities() {
        let server = v2::ServerInfo {
            name: "ddb".to_string(),
            version: "test".to_string(),
            server_instance_id: "server".to_string(),
            api_versions: vec!["v2".to_string()],
            ..Default::default()
        };
        assert!(validate_discovery(&server, &capabilities()).is_ok());
        let mut mismatched = capabilities();
        mismatched.server_instance_id = "other".to_string();
        assert!(validate_discovery(&server, &mismatched).is_err());
    }

    #[test]
    fn endpoint_diagnostics_strip_query_and_reject_credentials() {
        assert_eq!(
            safe_endpoint("https://debug.example/prefix?secret=value#fragment").unwrap(),
            "https://debug.example/prefix"
        );
        assert!(safe_endpoint("https://user:secret@debug.example").is_err());
    }

    #[test]
    fn duplicate_or_empty_public_ids_fail_validation() {
        assert!(unique_strings(["one", "two"], "ids").is_ok());
        assert!(unique_strings(["one", "one"], "ids").is_err());
        assert!(unique_strings([""], "ids").is_err());
    }
}
