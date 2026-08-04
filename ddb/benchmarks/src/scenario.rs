use std::{
    fmt,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::json;

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
            | Self::CliThreadInfo
            | Self::CliBreakInsert
            | Self::Notifications
            | Self::DistributedBacktrace
            | Self::LldbDistributedBacktrace => "latency_ms",
        }
    }
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
    pub notification_subscribers: usize,
    pub warmup: usize,
    pub samples: usize,
    pub startup_warmup: usize,
    pub startup_samples: usize,
    pub lldb_eager_stack_warmup: bool,
}

#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub scenario: String,
    pub description: &'static str,
    pub metric: &'static str,
    pub sessions: usize,
    pub dbt_depth: Option<usize>,
    pub threads_per_session: usize,
    pub notification_subscribers: Option<usize>,
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

    let (sessions, dbt_depth, threads_per_session) = match kind {
        ScenarioKind::DistributedBacktrace | ScenarioKind::LldbDistributedBacktrace => {
            (scale, Some(scale), 1)
        }
        _ => (scale, None, config.threads_per_session),
    };

    Ok(ScenarioResult {
        scenario: kind.as_str().to_string(),
        description: kind.description(),
        metric: kind.metric(),
        sessions,
        dbt_depth,
        threads_per_session,
        notification_subscribers: matches!(kind, ScenarioKind::Notifications)
            .then_some(config.notification_subscribers),
        stats,
    })
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
