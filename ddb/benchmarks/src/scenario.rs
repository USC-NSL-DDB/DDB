use std::{
    collections::HashSet,
    fmt,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use ddb_api_client::{ClientConfig, DdbClient, ProjectedStateSyncItem, StateSyncOptions};
use ddb_api_grpc::v2::{
    ddb_event_service_client::DdbEventServiceClient,
    debugger_control_service_client::DebuggerControlServiceClient,
    debugger_service_client::DebuggerServiceClient,
};
use ddb_api_types::v2;
use serde::Serialize;
use serde_json::json;
use tonic::{metadata::Ascii, metadata::MetadataValue, transport::Endpoint, Request};

use crate::{
    harness::{dbt_session_tag, session_id_by_tag, DdbHarness, HarnessSpec, RealDebugger},
    stats::SummaryStats,
};

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioKind {
    Startup,
    ApiThreadInfo,
    ApiThreadInfoBurst,
    ApiListGroups,
    V2HttpSnapshot,
    V2GrpcSnapshot,
    V2HttpStepStop,
    V2GrpcStepStop,
    V2HttpDrainedOutputStep,
    V2HttpMixedOutputStep,
    V2HttpVariableInspection,
    V2HttpMemoryTransfer,
    V2HttpStateFanout,
    V2HttpReconnectReplay,
    CliThreadInfo,
    CliBreakInsert,
    Notifications,
    DistributedBacktrace,
    LldbDistributedBacktrace,
}

impl ScenarioKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::ApiThreadInfo => "api-thread-info",
            Self::ApiThreadInfoBurst => "api-thread-info-burst",
            Self::ApiListGroups => "api-list-groups",
            Self::V2HttpSnapshot => "v2-http-snapshot",
            Self::V2GrpcSnapshot => "v2-grpc-snapshot",
            Self::V2HttpStepStop => "v2-http-step-stop",
            Self::V2GrpcStepStop => "v2-grpc-step-stop",
            Self::V2HttpDrainedOutputStep => "v2-http-drained-output-step",
            Self::V2HttpMixedOutputStep => "v2-http-mixed-output-step",
            Self::V2HttpVariableInspection => "v2-http-variable-inspection",
            Self::V2HttpMemoryTransfer => "v2-http-memory-transfer",
            Self::V2HttpStateFanout => "v2-http-state-fanout",
            Self::V2HttpReconnectReplay => "v2-http-reconnect-replay",
            Self::CliThreadInfo => "cli-thread-info",
            Self::CliBreakInsert => "cli-break-insert",
            Self::Notifications => "notifications",
            Self::DistributedBacktrace => "distributed-backtrace",
            Self::LldbDistributedBacktrace => "lldb-distributed-backtrace",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Startup => {
                "Cold-start attach path for static mock sessions, including session registration and router wiring."
            }
            Self::ApiThreadInfo => {
                "HTTP /send round-trip for a broadcast -thread-info command, exercising router fanout and session-runtime aggregation."
            }
            Self::ApiThreadInfoBurst => {
                "N concurrent HTTP requests each broadcast -thread-info to N sessions, stressing admission, per-session pipelining, correlation, and fanout."
            }
            Self::ApiListGroups => {
                "HTTP /send round-trip for -list-thread-groups, covering broadcast fanout plus process/group response aggregation."
            }
            Self::V2HttpSnapshot => {
                "Full public v2 snapshot over a reused HTTP/ProtoJSON connection, including topology, execution, breakpoints, operations, extensions, and capabilities."
            }
            Self::V2GrpcSnapshot => {
                "The identical full public v2 snapshot over a reused Tonic/Protobuf connection to the opt-in native preview listener."
            }
            Self::V2HttpStepStop => {
                "Typed NEXT admission through HTTP/ProtoJSON until the corresponding replayable stopped execution event is observed."
            }
            Self::V2GrpcStepStop => {
                "The identical typed NEXT-to-stopped workflow over reused Tonic/Protobuf unary and state-stream connections."
            }
            Self::V2HttpDrainedOutputStep => {
                "Typed HTTP NEXT-to-stopped latency while an identical bounded output load is actively drained by a separate consumer."
            }
            Self::V2HttpMixedOutputStep => {
                "Typed HTTP NEXT-to-stopped latency while a separate unread output subscription is flooded through the bounded output lane."
            }
            Self::V2HttpVariableInspection => {
                "Bounded public HTTP variable pages projected repeatedly until the configured large-inspection variable count is delivered."
            }
            Self::V2HttpMemoryTransfer => {
                "Configured MiB transferred through repeated bounded public HTTP ReadMemory chunks."
            }
            Self::V2HttpStateFanout => {
                "Typed NEXT-to-stopped latency until every concurrent public state subscriber observes the same stop."
            }
            Self::V2HttpReconnectReplay => {
                "SDK-owned forced reconnect and replay convergence while typed NEXT operations advance execution state."
            }
            Self::CliThreadInfo => {
                "CLI command-handler latency for -thread-info, including parse, handler dispatch, routing, aggregation, and stdout emission."
            }
            Self::CliBreakInsert => {
                "CLI breakpoint insertion against a group target, covering group resolution, breakpoint manager mutation, fanout, and response formatting."
            }
            Self::Notifications => {
                "WebSocket broadcast latency for test notifications, covering notification serialization and subscriber fanout."
            }
            Self::DistributedBacktrace => {
                "Real GDB-backed end-to-end distributed backtrace latency for a synthetic cross-process call chain, including parent metadata lookup, interrupt, context switch, and recursive stack aggregation."
            }
            Self::LldbDistributedBacktrace => {
                "Real LLDB-backed end-to-end distributed backtrace latency for the same synthetic cross-process call chain and backend-neutral command path."
            }
        }
    }

    pub fn metric(&self) -> &'static str {
        match self {
            Self::Startup => "cold_start_ms",
            Self::ApiThreadInfo
            | Self::ApiThreadInfoBurst
            | Self::ApiListGroups
            | Self::V2HttpSnapshot
            | Self::V2GrpcSnapshot
            | Self::V2HttpStepStop
            | Self::V2GrpcStepStop
            | Self::V2HttpDrainedOutputStep
            | Self::V2HttpMixedOutputStep
            | Self::V2HttpVariableInspection
            | Self::V2HttpMemoryTransfer
            | Self::V2HttpStateFanout
            | Self::V2HttpReconnectReplay
            | Self::CliThreadInfo
            | Self::CliBreakInsert
            | Self::Notifications
            | Self::DistributedBacktrace
            | Self::LldbDistributedBacktrace => "latency_ms",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OutputLoad {
    None,
    Drained,
    Unread,
}

impl fmt::Display for ScenarioKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct ScenarioConfig<'a> {
    pub binary: &'a Path,
    pub workspace_root: &'a Path,
    pub timeout: Duration,
    pub threads_per_session: usize,
    pub variables_per_frame: usize,
    pub memory_chunk_bytes: usize,
    pub notification_subscribers: usize,
    pub warmup: usize,
    pub samples: usize,
    pub startup_warmup: usize,
    pub startup_samples: usize,
    pub lldb_eager_stack_warmup: bool,
    pub bulk_output_events: usize,
    pub bulk_output_event_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub scenario: String,
    pub description: &'static str,
    pub metric: &'static str,
    pub sessions: usize,
    pub scale: usize,
    pub scale_unit: &'static str,
    pub dbt_depth: Option<usize>,
    pub threads_per_session: usize,
    pub notification_subscribers: Option<usize>,
    pub bulk_output_events: Option<usize>,
    pub bulk_output_event_bytes: Option<usize>,
    pub bulk_output_total_bytes: Option<u64>,
    pub stats: SummaryStats,
}

pub fn run_scenario(
    kind: ScenarioKind,
    scale: usize,
    config: &ScenarioConfig<'_>,
) -> Result<ScenarioResult> {
    let stats =
        match kind {
            ScenarioKind::Startup => SummaryStats::from_durations(&measure_startup(scale, config)?),
            ScenarioKind::ApiThreadInfo => SummaryStats::from_durations(
                &measure_with_live_harness(scale, config, |harness, _, _| {
                    let start = Instant::now();
                    let response = harness.api_post_json(
                        "/send",
                        &json!({
                            "wait": true,
                            "cmd": "-thread-info",
                        }),
                    )?;
                    let payload = response["payload"]
                        .as_object()
                        .context("missing /send payload for api-thread-info")?;
                    let responses = payload
                        .get("responses")
                        .and_then(|value| value.as_array())
                        .context("missing responses array for api-thread-info")?;
                    if responses.len() != scale {
                        bail!(
                            "expected {} session responses for api-thread-info, got {}",
                            scale,
                            responses.len()
                        );
                    }
                    Ok(start.elapsed())
                })?,
            ),
            ScenarioKind::ApiThreadInfoBurst => SummaryStats::from_durations(
                &measure_with_live_harness(scale, config, |harness, _, _| {
                    let bodies = (0..scale)
                        .map(|_| {
                            json!({
                                "wait": true,
                                "cmd": "-thread-info",
                            })
                        })
                        .collect::<Vec<_>>();
                    let start = Instant::now();
                    let responses =
                        harness.api_post_json_concurrent("/send", bodies, config.timeout)?;
                    for response in responses {
                        let response_count = response["payload"]["responses"]
                            .as_array()
                            .map(Vec::len)
                            .context("missing burst thread-info responses")?;
                        if response_count != scale {
                            bail!(
                                "expected {} responses per burst request, got {}",
                                scale,
                                response_count
                            );
                        }
                    }
                    Ok(start.elapsed())
                })?,
            ),
            ScenarioKind::ApiListGroups => SummaryStats::from_durations(
                &measure_with_live_harness(scale, config, |harness, _, _| {
                    let start = Instant::now();
                    let response = harness.api_post_json(
                        "/send",
                        &json!({
                            "wait": true,
                            "cmd": "-list-thread-groups",
                        }),
                    )?;
                    let payload = response["payload"]
                        .as_object()
                        .context("missing /send payload for api-list-groups")?;
                    let responses = payload
                        .get("responses")
                        .and_then(|value| value.as_array())
                        .context("missing responses array for api-list-groups")?;
                    if responses.len() != scale {
                        bail!(
                            "expected {} session responses for api-list-groups, got {}",
                            scale,
                            responses.len()
                        );
                    }
                    Ok(start.elapsed())
                })?,
            ),
            ScenarioKind::V2HttpSnapshot => {
                SummaryStats::from_durations(&measure_v2_http_snapshot(scale, config)?)
            }
            ScenarioKind::V2GrpcSnapshot => {
                SummaryStats::from_durations(&measure_v2_grpc_snapshot(scale, config)?)
            }
            ScenarioKind::V2HttpStepStop => SummaryStats::from_durations(
                &measure_v2_http_step_stop(scale, config, OutputLoad::None)?,
            ),
            ScenarioKind::V2GrpcStepStop => {
                SummaryStats::from_durations(&measure_v2_grpc_step_stop(scale, config)?)
            }
            ScenarioKind::V2HttpDrainedOutputStep => SummaryStats::from_durations(
                &measure_v2_http_step_stop(scale, config, OutputLoad::Drained)?,
            ),
            ScenarioKind::V2HttpMixedOutputStep => SummaryStats::from_durations(
                &measure_v2_http_step_stop(scale, config, OutputLoad::Unread)?,
            ),
            ScenarioKind::V2HttpVariableInspection => {
                SummaryStats::from_durations(&measure_v2_http_variable_inspection(scale, config)?)
            }
            ScenarioKind::V2HttpMemoryTransfer => {
                SummaryStats::from_durations(&measure_v2_http_memory_transfer(scale, config)?)
            }
            ScenarioKind::V2HttpStateFanout => {
                SummaryStats::from_durations(&measure_v2_http_state_fanout(scale, config)?)
            }
            ScenarioKind::V2HttpReconnectReplay => {
                SummaryStats::from_durations(&measure_v2_http_reconnect_replay(scale, config)?)
            }
            ScenarioKind::CliThreadInfo => SummaryStats::from_durations(
                &measure_with_live_harness(scale, config, |harness, iteration, sample_idx| {
                    let token = 10_000_u64 + iteration as u64;
                    let cursor = harness.send_cli_cmd(&format!("{token}-thread-info"))?;
                    let start = Instant::now();
                    let line = harness.wait_for_stdout_match(cursor, config.timeout, |line| {
                        line.starts_with(&format!("{token}^done"))
                    })?;
                    if !line.contains("threads") {
                        bail!(
                            "thread-info benchmark sample {} returned unexpected output: {}",
                            sample_idx,
                            line
                        );
                    }
                    Ok(start.elapsed())
                })?,
            ),
            ScenarioKind::CliBreakInsert => SummaryStats::from_durations(
                &measure_with_live_harness(scale, config, |harness, iteration, _| {
                    let gid = harness.first_group_id()?;
                    let token = 20_000_u64 + iteration as u64;
                    let line_number = 1_000_u64 + iteration as u64;
                    let cursor = harness.send_cli_cmd(&format!(
                    "{token}-break-insert bench_{scale}_{iteration}.rs:{line_number} --group {gid}"
                ))?;
                    let start = Instant::now();
                    let line = harness.wait_for_stdout_match(cursor, config.timeout, |line| {
                        line.starts_with(&format!("{token}^done"))
                    })?;
                    if !line.contains("done") {
                        bail!(
                            "break-insert benchmark returned unexpected output: {}",
                            line
                        );
                    }
                    Ok(start.elapsed())
                })?,
            ),
            ScenarioKind::Notifications => SummaryStats::from_durations(
                &measure_with_live_harness(scale, config, |harness, iteration, _| {
                    let subscribers = harness.connect_notification_subscribers(
                        config.notification_subscribers,
                        config.timeout,
                    )?;
                    harness.wait_for_notification_subscribers(
                        config.notification_subscribers,
                        config.timeout,
                    )?;

                    let message = format!("bench-notification-{scale}-{iteration}");
                    let start = Instant::now();
                    let response = harness.post_test_notification(&message)?;
                    if response["success"].as_bool() != Some(true) {
                        bail!("notification test endpoint returned unsuccessful response");
                    }
                    subscribers.wait_for_all(&message, config.timeout)?;
                    let elapsed = start.elapsed();

                    harness.wait_for_notification_subscribers(0, config.timeout)?;
                    Ok(elapsed)
                })?,
            ),
            ScenarioKind::DistributedBacktrace => SummaryStats::from_durations(
                &measure_distributed_backtrace(RealDebugger::Gdb, scale, config)?,
            ),
            ScenarioKind::LldbDistributedBacktrace => SummaryStats::from_durations(
                &measure_distributed_backtrace(RealDebugger::Lldb, scale, config)?,
            ),
        };

    let (sessions, dbt_depth, threads_per_session, scale_unit) = match kind {
        ScenarioKind::DistributedBacktrace | ScenarioKind::LldbDistributedBacktrace => {
            (scale, Some(scale), 1, "depth")
        }
        ScenarioKind::V2HttpVariableInspection => (1, None, 1, "variables"),
        ScenarioKind::V2HttpMemoryTransfer => (1, None, 1, "MiB"),
        ScenarioKind::V2HttpStateFanout => (1, None, 1, "subscribers"),
        _ => (scale, None, config.threads_per_session, "sessions"),
    };

    Ok(ScenarioResult {
        scenario: kind.as_str().to_string(),
        description: kind.description(),
        metric: kind.metric(),
        sessions,
        scale,
        scale_unit,
        dbt_depth,
        threads_per_session,
        notification_subscribers: matches!(kind, ScenarioKind::Notifications)
            .then_some(config.notification_subscribers),
        bulk_output_events: uses_bulk_output(kind).then_some(config.bulk_output_events),
        bulk_output_event_bytes: uses_bulk_output(kind).then_some(config.bulk_output_event_bytes),
        bulk_output_total_bytes: uses_bulk_output(kind).then_some(
            (config.bulk_output_events as u64)
                .saturating_mul(config.bulk_output_event_bytes as u64),
        ),
        stats,
    })
}

fn uses_bulk_output(kind: ScenarioKind) -> bool {
    matches!(
        kind,
        ScenarioKind::V2HttpDrainedOutputStep | ScenarioKind::V2HttpMixedOutputStep
    )
}

fn measure_distributed_backtrace(
    debugger: RealDebugger,
    depth: usize,
    config: &ScenarioConfig<'_>,
) -> Result<Vec<Duration>> {
    let mut samples = Vec::with_capacity(config.samples);
    for iteration in 0..(config.warmup + config.samples) {
        let mut harness = DdbHarness::spawn_real_dbt(
            config.binary,
            config.workspace_root,
            debugger,
            depth,
            config.lldb_eager_stack_warmup,
        )?;
        harness.wait_for_status_up(config.timeout)?;
        let sessions = harness.provision_real_dbt_contexts(depth, config.timeout)?;

        let leaf_sid = session_id_by_tag(&sessions, &dbt_session_tag(depth))?;
        let root_sid = session_id_by_tag(&sessions, &dbt_session_tag(1))?;
        let leaf_gtid = harness.resolve_single_thread_gtid(leaf_sid, config.timeout)?;

        let token = 90_000_u64 + iteration as u64;
        let start = Instant::now();
        let cursor = harness.send_cli_cmd(&format!("{token}-bt-remote --thread {leaf_gtid}"))?;
        let line = harness.wait_for_stdout_match(cursor, config.timeout, |line| {
            is_terminal_result(line, token)
        })?;
        let elapsed = start.elapsed();

        if line.starts_with(&format!("{token}^error")) {
            bail!(
                "distributed-backtrace command failed at depth {}: {}",
                depth,
                line
            );
        }

        if !line.contains(&format!("session=\"{leaf_sid}\"")) {
            bail!(
                "distributed-backtrace output missing leaf session {} at depth {}: {}",
                leaf_sid,
                depth,
                line
            );
        }
        if !line.contains(&format!("session=\"{root_sid}\"")) {
            bail!(
                "distributed-backtrace output missing root session {} at depth {}: {}",
                root_sid,
                depth,
                line
            );
        }
        let boundary_frames = line.matches("boundary_frame=\"1\"").count();
        if boundary_frames != depth.saturating_sub(1) {
            bail!(
                "distributed-backtrace output had {} boundary frames, expected {} at depth {}: {}",
                boundary_frames,
                depth.saturating_sub(1),
                depth,
                line
            );
        }

        if iteration >= config.warmup {
            samples.push(elapsed);
        }
    }

    Ok(samples)
}

fn is_terminal_result(line: &str, token: u64) -> bool {
    let prefix = format!("{token}^");
    line.strip_prefix(&prefix)
        .is_some_and(|result| result.starts_with("done") || result.starts_with("error"))
}

fn measure_startup(sessions: usize, config: &ScenarioConfig<'_>) -> Result<Vec<Duration>> {
    let mut samples = Vec::with_capacity(config.startup_samples);
    for iteration in 0..(config.startup_warmup + config.startup_samples) {
        let start = Instant::now();
        let mut harness = DdbHarness::spawn(
            config.binary,
            config.workspace_root,
            HarnessSpec {
                sessions,
                threads_per_session: config.threads_per_session,
                variables_per_frame: 2,
                exit_on_continue: false,
            },
        )?;
        harness.wait_for_status_up(config.timeout)?;
        harness.wait_for_sessions_len(sessions, config.timeout)?;
        let elapsed = start.elapsed();
        if iteration >= config.startup_warmup {
            samples.push(elapsed);
        }
    }
    Ok(samples)
}

fn measure_with_live_harness<F>(
    sessions: usize,
    config: &ScenarioConfig<'_>,
    mut measure: F,
) -> Result<Vec<Duration>>
where
    F: FnMut(&mut DdbHarness, usize, usize) -> Result<Duration>,
{
    let mut harness = DdbHarness::spawn(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions,
            threads_per_session: config.threads_per_session,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(sessions, config.timeout)?;

    let mut samples = Vec::with_capacity(config.samples);
    for iteration in 0..(config.warmup + config.samples) {
        let duration = measure(&mut harness, iteration, samples.len())?;
        if iteration >= config.warmup {
            samples.push(duration);
        }
    }
    Ok(samples)
}

fn measure_v2_http_snapshot(sessions: usize, config: &ScenarioConfig<'_>) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions,
            threads_per_session: config.threads_per_session,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(sessions, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create HTTP benchmark runtime")?;
    let client = DdbClient::new(
        ClientConfig::new(harness.http_endpoint())
            .with_bearer_token(harness.api_token()?.to_string()),
    )?;
    runtime
        .block_on(client.handshake())
        .context("v2 HTTP benchmark handshake failed")?;

    let mut samples = Vec::with_capacity(config.samples);
    for iteration in 0..(config.warmup + config.samples) {
        let start = Instant::now();
        let response = runtime
            .block_on(client.get_snapshot(full_snapshot_request()))
            .context("v2 HTTP snapshot request failed")?;
        let elapsed = start.elapsed();
        validate_snapshot(
            response.snapshot.as_ref(),
            sessions,
            config.threads_per_session,
        )?;
        if iteration >= config.warmup {
            samples.push(elapsed);
        }
    }
    Ok(samples)
}

fn measure_v2_http_variable_inspection(
    total_variables: usize,
    config: &ScenarioConfig<'_>,
) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions: 1,
            threads_per_session: 1,
            variables_per_frame: config.variables_per_frame,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(1, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create variable benchmark runtime")?;
    let client = DdbClient::new(
        ClientConfig::new(harness.http_endpoint())
            .with_bearer_token(harness.api_token()?.to_string()),
    )?;
    runtime
        .block_on(client.handshake())
        .context("v2 variable benchmark handshake failed")?;

    runtime.block_on(async {
        let snapshot = client
            .get_snapshot(full_snapshot_request())
            .await
            .context("variable benchmark snapshot failed")?
            .snapshot
            .context("GetSnapshot omitted snapshot")?;
        let thread_id = snapshot
            .threads
            .first()
            .context("variable benchmark requires one thread")?
            .thread_id
            .clone();
        let frames = client
            .collect_frames(
                v2::ListFramesRequest {
                    thread_id,
                    page: Some(v2::PageRequest {
                        page_size: 1,
                        page_token: None,
                    }),
                    ..Default::default()
                },
                1,
            )
            .await
            .context("collect benchmark frame")?;
        let frame_id = frames
            .first()
            .context("variable benchmark frame was absent")?
            .frame_id
            .clone();
        let scopes = client
            .collect_scopes(
                v2::ListScopesRequest {
                    frame_id,
                    page: Some(v2::PageRequest {
                        page_size: 1,
                        page_token: None,
                    }),
                    ..Default::default()
                },
                1,
            )
            .await
            .context("collect benchmark scope")?;
        let scope_id = scopes
            .first()
            .context("variable benchmark scope was absent")?
            .scope_id
            .clone();

        let mut samples = Vec::with_capacity(config.samples);
        for iteration in 0..(config.warmup + config.samples) {
            let start = Instant::now();
            let mut delivered = 0_usize;
            while delivered < total_variables {
                let batch_size = (total_variables - delivered).min(config.variables_per_frame);
                let batch_delivered = if batch_size == config.variables_per_frame {
                    let variables = client
                        .collect_variables(
                            v2::ListVariablesRequest {
                                scope_id: scope_id.clone(),
                                page: Some(v2::PageRequest {
                                    page_size: batch_size.min(200) as u32,
                                    page_token: None,
                                }),
                                ..Default::default()
                            },
                            batch_size,
                        )
                        .await
                        .context("collect bounded variable workload")?;
                    if variables.len() != batch_size {
                        bail!(
                            "variable workload returned {} roots, expected {}",
                            variables.len(),
                            batch_size
                        );
                    }
                    variables.len()
                } else {
                    collect_variable_prefix(&client, &scope_id, batch_size).await?
                };
                delivered = delivered.saturating_add(batch_delivered);
            }
            let elapsed = start.elapsed();
            if delivered != total_variables {
                bail!("variable workload delivered {delivered}, expected {total_variables}");
            }
            if iteration >= config.warmup {
                samples.push(elapsed);
            }
        }
        Ok(samples)
    })
}

async fn collect_variable_prefix(
    client: &DdbClient,
    scope_id: &str,
    max_items: usize,
) -> Result<usize> {
    let mut delivered = 0_usize;
    let mut page_token = None;
    let mut seen_tokens = HashSet::new();
    while delivered < max_items {
        let requested = (max_items - delivered).min(200);
        let response = client
            .list_variables(v2::ListVariablesRequest {
                scope_id: scope_id.to_string(),
                page: Some(v2::PageRequest {
                    page_size: requested as u32,
                    page_token,
                }),
                ..Default::default()
            })
            .await
            .context("collect final bounded variable page")?;
        let count = response.variables.len();
        if count == 0 || count > requested {
            bail!("variable page returned {count} roots, expected 1..={requested}");
        }
        delivered += count;
        if delivered == max_items {
            break;
        }
        let next_token = response
            .page
            .and_then(|page| page.next_page_token)
            .filter(|token| !token.is_empty())
            .context("variable collection ended before the requested prefix")?;
        if !seen_tokens.insert(next_token.clone()) {
            bail!("variable collection repeated a continuation token");
        }
        page_token = Some(next_token);
    }
    Ok(delivered)
}

fn measure_v2_http_memory_transfer(
    transfer_mib: usize,
    config: &ScenarioConfig<'_>,
) -> Result<Vec<Duration>> {
    let total_bytes = transfer_mib
        .checked_mul(1024 * 1024)
        .context("memory transfer size overflowed")?;
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions: 1,
            threads_per_session: 1,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(1, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create memory benchmark runtime")?;
    let client = DdbClient::new(
        ClientConfig::new(harness.http_endpoint())
            .with_bearer_token(harness.api_token()?.to_string()),
    )?;
    runtime
        .block_on(client.handshake())
        .context("v2 memory benchmark handshake failed")?;

    runtime.block_on(async {
        let snapshot = client
            .get_snapshot(full_snapshot_request())
            .await
            .context("memory benchmark snapshot failed")?
            .snapshot
            .context("GetSnapshot omitted snapshot")?;
        let target = thread_target(
            &snapshot
                .threads
                .first()
                .context("memory benchmark requires one thread")?
                .thread_id,
        );

        let mut samples = Vec::with_capacity(config.samples);
        for iteration in 0..(config.warmup + config.samples) {
            let start = Instant::now();
            let mut remaining = total_bytes;
            let mut delivered = 0_usize;
            while remaining > 0 {
                let chunk = remaining.min(config.memory_chunk_bytes);
                let address = 0x1000_usize
                    .checked_add(delivered)
                    .context("memory workload address overflowed")?;
                let response = client
                    .read_memory(v2::ReadMemoryRequest {
                        target: Some(target.clone()),
                        address: format!("0x{address:x}"),
                        byte_count: chunk as u64,
                        ..Default::default()
                    })
                    .await
                    .context("read bounded memory workload chunk")?;
                let memory = response.memory.context("ReadMemory omitted memory")?;
                if memory.data.len() != chunk || memory.unreadable_bytes != 0 {
                    bail!(
                        "memory chunk returned {} readable and {} unreadable bytes, expected {} readable",
                        memory.data.len(),
                        memory.unreadable_bytes,
                        chunk
                    );
                }
                delivered = delivered.saturating_add(memory.data.len());
                remaining -= chunk;
            }
            let elapsed = start.elapsed();
            if delivered != total_bytes {
                bail!("memory workload delivered {delivered}, expected {total_bytes}");
            }
            if iteration >= config.warmup {
                samples.push(elapsed);
            }
        }
        Ok(samples)
    })
}

fn measure_v2_http_state_fanout(
    subscriber_count: usize,
    config: &ScenarioConfig<'_>,
) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions: 1,
            threads_per_session: 1,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(1, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create fanout benchmark runtime")?;
    let client = DdbClient::new(
        ClientConfig::new(harness.http_endpoint())
            .with_bearer_token(harness.api_token()?.to_string()),
    )?;
    runtime
        .block_on(client.handshake())
        .context("v2 fanout benchmark handshake failed")?;

    runtime.block_on(async {
        let snapshot = client
            .get_snapshot(full_snapshot_request())
            .await
            .context("fanout benchmark snapshot failed")?
            .snapshot
            .context("GetSnapshot omitted snapshot")?;
        let thread = snapshot
            .threads
            .first()
            .context("fanout benchmark requires one thread")?;
        let thread_id = thread.thread_id.clone();
        let target = thread_target(&thread_id);
        let mut current_line = thread.location.as_ref().map_or(0, |location| location.line);
        let mut subscribers = Vec::with_capacity(subscriber_count);
        for _ in 0..subscriber_count {
            subscribers.push(
                client
                    .subscribe_state_events(execution_subscription(
                        snapshot.state_event_cursor.clone(),
                    ))
                    .await
                    .context("subscribe fanout benchmark consumer")?,
            );
        }

        let mut samples = Vec::with_capacity(config.samples);
        for iteration in 0..(config.warmup + config.samples) {
            let previous_line = current_line;
            let start = Instant::now();
            let admission = client
                .execute(v2::ExecuteRequest {
                    target: Some(target.clone()),
                    action: v2::ExecutionAction::Next as i32,
                    ..Default::default()
                })
                .await
                .context("admit fanout benchmark NEXT")?;
            let operation_id = admission_operation_id(admission, "fanout NEXT")?;
            let mut observed_line = None;
            for subscriber in &mut subscribers {
                let line = wait_for_http_stopped_event(
                    subscriber,
                    &operation_id,
                    &thread_id,
                    previous_line,
                    config.timeout,
                )
                .await?;
                if observed_line.is_some_and(|expected| expected != line) {
                    bail!("state subscribers disagreed on stopped line");
                }
                observed_line = Some(line);
            }
            current_line = observed_line.context("no state subscriber observed the stop")?;
            let elapsed = start.elapsed();
            ensure_http_operation_completed(&client, &operation_id, config.timeout).await?;
            if iteration >= config.warmup {
                samples.push(elapsed);
            }
        }
        Ok(samples)
    })
}

fn measure_v2_http_reconnect_replay(
    sessions: usize,
    config: &ScenarioConfig<'_>,
) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions,
            threads_per_session: config.threads_per_session,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(sessions, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create reconnect benchmark runtime")?;
    let client = DdbClient::new(
        ClientConfig::new(harness.http_endpoint())
            .with_bearer_token(harness.api_token()?.to_string()),
    )?;
    runtime
        .block_on(client.handshake())
        .context("v2 reconnect benchmark handshake failed")?;

    runtime.block_on(async {
        let mut sync = client.projected_state_sync(StateSyncOptions {
            sections: full_snapshot_request().sections,
            filter: execution_subscription(None).filter,
            reconnect_initial_delay: Duration::from_millis(1),
            reconnect_max_delay: Duration::from_millis(1),
            max_reconnect_attempts: Some(3),
            ..Default::default()
        })?;
        if !matches!(sync.next().await?, ProjectedStateSyncItem::Snapshot) {
            bail!("projected reconnect workflow did not begin with a snapshot");
        }
        let snapshot = sync
            .current_snapshot()
            .context("projected reconnect workflow omitted its snapshot")?;
        validate_snapshot(Some(&snapshot), sessions, config.threads_per_session)?;
        let thread = snapshot
            .threads
            .first()
            .context("reconnect benchmark requires one thread")?;
        let thread_id = thread.thread_id.clone();
        let target = thread_target(&thread_id);
        let mut current_line = thread.location.as_ref().map_or(0, |location| location.line);

        let mut samples = Vec::with_capacity(config.samples);
        for iteration in 0..(config.warmup + config.samples) {
            sync.force_reconnect();
            let start = Instant::now();
            let admission = client
                .execute(v2::ExecuteRequest {
                    target: Some(target.clone()),
                    action: v2::ExecutionAction::Next as i32,
                    ..Default::default()
                })
                .await
                .context("admit reconnect benchmark NEXT")?;
            let operation_id = admission_operation_id(admission, "reconnect NEXT")?;
            ensure_http_operation_completed(&client, &operation_id, config.timeout).await?;

            let next_line = tokio::time::timeout(config.timeout, async {
                loop {
                    match sync.next().await? {
                        ProjectedStateSyncItem::Event(event) => {
                            if let Some(line) =
                                stopped_line(&event, &operation_id, &thread_id, current_line)
                            {
                                return Ok::<_, anyhow::Error>(line);
                            }
                        }
                        ProjectedStateSyncItem::Snapshot => {
                            bail!("forced reconnect unexpectedly rehydrated a fresh snapshot")
                        }
                        ProjectedStateSyncItem::Rehydrating { reason } => {
                            bail!("forced reconnect required rehydration: {reason:?}")
                        }
                        ProjectedStateSyncItem::Reconnecting { .. } => {}
                    }
                }
            })
            .await
            .context("timed out waiting for reconnect replay convergence")??;
            current_line = next_line;
            let elapsed = start.elapsed();
            if iteration >= config.warmup {
                samples.push(elapsed);
            }
        }
        Ok(samples)
    })
}

fn measure_v2_grpc_snapshot(sessions: usize, config: &ScenarioConfig<'_>) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions,
            threads_per_session: config.threads_per_session,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(sessions, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create gRPC benchmark runtime")?;
    let endpoint = Endpoint::from_shared(harness.grpc_endpoint()?)?
        .connect_timeout(config.timeout)
        .timeout(config.timeout);
    let channel = runtime
        .block_on(endpoint.connect())
        .context("connect gRPC benchmark client")?;
    let authorization: MetadataValue<Ascii> = format!("Bearer {}", harness.api_token()?)
        .parse()
        .context("construct gRPC authorization metadata")?;
    let mut client =
        DebuggerServiceClient::with_interceptor(channel, move |mut request: Request<()>| {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
            Ok(request)
        });
    runtime
        .block_on(client.get_capabilities(v2::GetCapabilitiesRequest::default()))
        .context("v2 gRPC benchmark handshake failed")?;

    let mut samples = Vec::with_capacity(config.samples);
    for iteration in 0..(config.warmup + config.samples) {
        let start = Instant::now();
        let response = runtime
            .block_on(client.get_snapshot(full_snapshot_request()))
            .context("v2 gRPC snapshot request failed")?
            .into_inner();
        let elapsed = start.elapsed();
        validate_snapshot(
            response.snapshot.as_ref(),
            sessions,
            config.threads_per_session,
        )?;
        if iteration >= config.warmup {
            samples.push(elapsed);
        }
    }
    Ok(samples)
}

fn measure_v2_http_step_stop(
    sessions: usize,
    config: &ScenarioConfig<'_>,
    output_load: OutputLoad,
) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions,
            threads_per_session: config.threads_per_session,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(sessions, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create HTTP benchmark runtime")?;
    let client = DdbClient::new(
        ClientConfig::new(harness.http_endpoint())
            .with_bearer_token(harness.api_token()?.to_string()),
    )?;
    runtime
        .block_on(client.handshake())
        .context("v2 HTTP benchmark handshake failed")?;

    runtime.block_on(async {
        let snapshot = client
            .get_snapshot(full_snapshot_request())
            .await
            .context("v2 HTTP step benchmark snapshot failed")?
            .snapshot
            .context("GetSnapshot omitted snapshot")?;
        let thread = snapshot
            .threads
            .first()
            .context("step benchmark requires at least one thread")?;
        let thread_id = thread.thread_id.clone();
        let mut stopped_line = thread.location.as_ref().map_or(0, |location| location.line);
        let target = thread_target(&thread_id);
        let mut state_events = client
            .subscribe_state_events(execution_subscription(snapshot.state_event_cursor.clone()))
            .await
            .context("subscribe to HTTP execution events")?;
        let mut output_stream = if output_load != OutputLoad::None {
            Some(
                client
                    .subscribe_output(v2::SubscribeOutputRequest::default())
                    .await
                    .context("subscribe HTTP output consumer")?,
            )
        } else {
            None
        };
        // The paired cases use identical producers. One drains the SDK stream;
        // the other holds the response body without polling so transport and
        // application queue backpressure cannot be confused with output work.
        let drain_task = if output_load == OutputLoad::Drained {
            let mut stream = output_stream.take().expect("drained stream exists");
            Some(tokio::spawn(async move {
                while matches!(stream.next().await, Ok(Some(_))) {}
            }))
        } else {
            None
        };
        let _slow_output = output_stream;

        let mut samples = Vec::with_capacity(config.samples);
        for iteration in 0..(config.warmup + config.samples) {
            if output_load != OutputLoad::None {
                let admission = client
                    .execute_raw_command(v2::ExecuteRawCommandRequest {
                        target: Some(target.clone()),
                        dialect: v2::RawCommandDialect::GdbMi as i32,
                        command: format!(
                            "-mock-start-output-stream {} {}",
                            config.bulk_output_events, config.bulk_output_event_bytes
                        ),
                        ..Default::default()
                    })
                    .await
                    .context("admit HTTP bulk-output operation")?;
                let operation_id = admission_operation_id(admission, "bulk output")?;
                ensure_http_operation_completed(&client, &operation_id, config.timeout).await?;
            }

            let start = Instant::now();
            let admission = client
                .execute(v2::ExecuteRequest {
                    target: Some(target.clone()),
                    action: v2::ExecutionAction::Next as i32,
                    ..Default::default()
                })
                .await
                .context("admit HTTP NEXT operation")?;
            let operation_id = admission_operation_id(admission, "NEXT")?;
            let stopped = wait_for_http_stopped_event(
                &mut state_events,
                &operation_id,
                &thread_id,
                stopped_line,
                config.timeout,
            )
            .await;
            stopped_line = match stopped {
                Ok(line) => line,
                Err(error) => {
                    let snapshot = client
                        .get_snapshot(full_snapshot_request())
                        .await
                        .context("obtain diagnostic snapshot after stop timeout")?
                        .snapshot
                        .context("diagnostic GetSnapshot omitted snapshot")?;
                    bail!(
                        "{error}; fresh execution states: {}",
                        execution_state_summaries(&snapshot.execution_states).join(", ")
                    );
                }
            };
            let elapsed = start.elapsed();

            ensure_http_operation_completed(&client, &operation_id, config.timeout).await?;
            if iteration >= config.warmup {
                samples.push(elapsed);
            }
        }
        if let Some(task) = drain_task {
            task.abort();
        }
        Ok(samples)
    })
}

fn measure_v2_grpc_step_stop(
    sessions: usize,
    config: &ScenarioConfig<'_>,
) -> Result<Vec<Duration>> {
    let mut harness = DdbHarness::spawn_v2_transports(
        config.binary,
        config.workspace_root,
        HarnessSpec {
            sessions,
            threads_per_session: config.threads_per_session,
            variables_per_frame: 2,
            exit_on_continue: false,
        },
    )?;
    harness.wait_for_status_up(config.timeout)?;
    harness.wait_for_sessions_len(sessions, config.timeout)?;

    let runtime = tokio::runtime::Runtime::new().context("create gRPC benchmark runtime")?;
    let endpoint = Endpoint::from_shared(harness.grpc_endpoint()?)?
        .connect_timeout(config.timeout)
        .timeout(config.timeout);
    let channel = runtime
        .block_on(endpoint.connect())
        .context("connect gRPC benchmark client")?;
    let authorization: MetadataValue<Ascii> = format!("Bearer {}", harness.api_token()?)
        .parse()
        .context("construct gRPC authorization metadata")?;
    let mut debugger = DebuggerServiceClient::with_interceptor(channel.clone(), {
        let authorization = authorization.clone();
        move |mut request: Request<()>| {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
            Ok(request)
        }
    });
    let mut control = DebuggerControlServiceClient::with_interceptor(channel.clone(), {
        let authorization = authorization.clone();
        move |mut request: Request<()>| {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
            Ok(request)
        }
    });
    let mut events =
        DdbEventServiceClient::with_interceptor(channel, move |mut request: Request<()>| {
            request
                .metadata_mut()
                .insert("authorization", authorization.clone());
            Ok(request)
        });

    runtime.block_on(async {
        debugger
            .get_capabilities(v2::GetCapabilitiesRequest::default())
            .await
            .context("v2 gRPC benchmark handshake failed")?;
        let snapshot = debugger
            .get_snapshot(full_snapshot_request())
            .await
            .context("v2 gRPC step benchmark snapshot failed")?
            .into_inner()
            .snapshot
            .context("GetSnapshot omitted snapshot")?;
        let thread = snapshot
            .threads
            .first()
            .context("step benchmark requires at least one thread")?;
        let thread_id = thread.thread_id.clone();
        let mut stopped_line = thread.location.as_ref().map_or(0, |location| location.line);
        let target = thread_target(&thread_id);
        let mut state_events = events
            .subscribe_state_events(execution_subscription(snapshot.state_event_cursor.clone()))
            .await
            .context("subscribe to gRPC execution events")?
            .into_inner();

        let mut samples = Vec::with_capacity(config.samples);
        for iteration in 0..(config.warmup + config.samples) {
            let start = Instant::now();
            let admission = control
                .execute(v2::ExecuteRequest {
                    context: Some(v2::RequestContext {
                        idempotency_key: Some(format!("bench-grpc-next-{iteration}")),
                        ..Default::default()
                    }),
                    target: Some(target.clone()),
                    action: v2::ExecutionAction::Next as i32,
                    ..Default::default()
                })
                .await
                .context("admit gRPC NEXT operation")?
                .into_inner();
            let operation_id = admission_operation_id(admission, "NEXT")?;
            stopped_line = wait_for_grpc_stopped_event(
                &mut state_events,
                &operation_id,
                &thread_id,
                stopped_line,
                config.timeout,
            )
            .await?;
            let elapsed = start.elapsed();
            if iteration >= config.warmup {
                samples.push(elapsed);
            }
        }
        Ok(samples)
    })
}

fn thread_target(thread_id: &str) -> v2::Target {
    v2::Target {
        selector: Some(v2::target::Selector::Thread(v2::ThreadTarget {
            thread_id: thread_id.to_string(),
        })),
    }
}

fn execution_subscription(after_cursor: Option<v2::Cursor>) -> v2::SubscribeStateEventsRequest {
    v2::SubscribeStateEventsRequest {
        after_cursor,
        filter: Some(v2::StateEventFilter {
            kinds: vec![v2::StateEventKind::ExecutionChanged as i32],
            resource_kinds: vec![v2::ResourceKind::ExecutionState as i32],
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn admission_operation_id(
    admission: v2::OperationAdmissionResponse,
    operation_name: &str,
) -> Result<String> {
    let operation = admission
        .operation
        .with_context(|| format!("{operation_name} admission omitted operation"))?;
    if operation.operation_id.is_empty() {
        bail!("{operation_name} admission returned an empty operation ID");
    }
    Ok(operation.operation_id)
}

fn stopped_line(
    event: &v2::StateEvent,
    operation_id: &str,
    thread_id: &str,
    previous_line: u32,
) -> Option<u32> {
    if event
        .operation_id
        .as_deref()
        .is_some_and(|cause| cause != operation_id)
    {
        return None;
    }
    let Some(v2::state_event::Payload::Upsert(v2::ResourceUpsert {
        resource:
            Some(v2::resource_upsert::Resource::ExecutionState(v2::ExecutionState {
                target:
                    Some(v2::Target {
                        selector: Some(v2::target::Selector::Thread(target)),
                    }),
                running: false,
                location: Some(location),
                ..
            })),
    })) = event.payload.as_ref()
    else {
        return None;
    };
    (target.thread_id == thread_id && location.line > previous_line).then_some(location.line)
}

async fn wait_for_http_stopped_event(
    events: &mut ddb_api_client::NdjsonStream<v2::StateEvent>,
    operation_id: &str,
    thread_id: &str,
    previous_line: u32,
    timeout: Duration,
) -> Result<u32> {
    let mut observed = Vec::new();
    let result = tokio::time::timeout(timeout, async {
        loop {
            let event = events
                .next()
                .await
                .context("read HTTP state event")?
                .context("HTTP state stream ended before stopped event")?;
            if let Some(line) = stopped_line(&event, operation_id, thread_id, previous_line) {
                return Ok::<_, anyhow::Error>(line);
            }
            if observed.len() < 32 {
                observed.push(state_event_summary(&event));
            }
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => bail!(
            "timed out waiting for HTTP stopped event; observed [{}]",
            observed.join(", ")
        ),
    }
}

async fn wait_for_grpc_stopped_event(
    events: &mut tonic::Streaming<v2::StateEvent>,
    operation_id: &str,
    thread_id: &str,
    previous_line: u32,
    timeout: Duration,
) -> Result<u32> {
    let mut observed = Vec::new();
    let result = tokio::time::timeout(timeout, async {
        loop {
            let event = events
                .message()
                .await
                .context("read gRPC state event")?
                .context("gRPC state stream ended before stopped event")?;
            if let Some(line) = stopped_line(&event, operation_id, thread_id, previous_line) {
                return Ok::<_, anyhow::Error>(line);
            }
            if observed.len() < 32 {
                observed.push(state_event_summary(&event));
            }
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => bail!(
            "timed out waiting for gRPC stopped event; observed [{}]",
            observed.join(", ")
        ),
    }
}

fn state_event_summary(event: &v2::StateEvent) -> String {
    let execution = match event.payload.as_ref() {
        Some(v2::state_event::Payload::Upsert(v2::ResourceUpsert {
            resource: Some(v2::resource_upsert::Resource::ExecutionState(execution)),
        })) => format!(
            "running={} line={:?} target={:?}",
            execution.running,
            execution.location.as_ref().map(|location| location.line),
            execution.target
        ),
        _ => "non-execution-payload".to_string(),
    };
    format!(
        "kind={} resource_kind={} cause={:?} {execution}",
        event.kind, event.resource_kind, event.operation_id
    )
}

fn execution_state_summaries(states: &[v2::ExecutionState]) -> Vec<String> {
    states
        .iter()
        .map(|execution| {
            format!(
                "running={} line={:?} target={:?}",
                execution.running,
                execution.location.as_ref().map(|location| location.line),
                execution.target
            )
        })
        .collect()
}

async fn ensure_http_operation_completed(
    client: &DdbClient,
    operation_id: &str,
    timeout: Duration,
) -> Result<()> {
    let operation = client
        .wait_operation(operation_id, timeout, Duration::from_millis(1))
        .await
        .with_context(|| format!("wait for operation {operation_id}"))?;
    if v2::OperationState::try_from(operation.state) != Ok(v2::OperationState::Completed) {
        bail!(
            "operation {} finished in unexpected state {}",
            operation.operation_id,
            operation.state
        );
    }
    Ok(())
}

fn full_snapshot_request() -> v2::GetSnapshotRequest {
    v2::GetSnapshotRequest {
        sections: vec![
            v2::SnapshotSection::Topology as i32,
            v2::SnapshotSection::Selection as i32,
            v2::SnapshotSection::Execution as i32,
            v2::SnapshotSection::Breakpoints as i32,
            v2::SnapshotSection::PendingOperations as i32,
            v2::SnapshotSection::Extensions as i32,
            v2::SnapshotSection::Capabilities as i32,
        ],
        ..Default::default()
    }
}

fn validate_snapshot(
    snapshot: Option<&v2::Snapshot>,
    sessions: usize,
    threads_per_session: usize,
) -> Result<()> {
    let snapshot = snapshot.context("GetSnapshot omitted snapshot")?;
    if snapshot.sessions.len() != sessions {
        bail!(
            "snapshot returned {} sessions, expected {sessions}",
            snapshot.sessions.len()
        );
    }
    let expected_threads = sessions.saturating_mul(threads_per_session);
    if snapshot.threads.len() != expected_threads {
        bail!(
            "snapshot returned {} threads, expected {expected_threads}",
            snapshot.threads.len()
        );
    }
    if snapshot.state_event_cursor.is_none() || snapshot.capabilities.is_none() {
        bail!("full snapshot omitted its replay cursor or capabilities");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_terminal_result;

    #[test]
    fn distributed_backtrace_wait_accepts_success_and_error_for_its_token() {
        assert!(is_terminal_result("90001^done,message=\"success\"", 90001));
        assert!(is_terminal_result("90001^error,msg=\"failed\"", 90001));
        assert!(!is_terminal_result("90002^done", 90001));
        assert!(!is_terminal_result("90001*stopped", 90001));
    }
}
