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
    harness::{DdbHarness, HarnessSpec},
    stats::SummaryStats,
};

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScenarioKind {
    Startup,
    ApiThreadInfo,
    ApiListGroups,
    CliThreadInfo,
    CliBreakInsert,
    Notifications,
}

impl ScenarioKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::ApiThreadInfo => "api-thread-info",
            Self::ApiListGroups => "api-list-groups",
            Self::CliThreadInfo => "cli-thread-info",
            Self::CliBreakInsert => "cli-break-insert",
            Self::Notifications => "notifications",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Startup => {
                "Cold-start attach path for static mock sessions, including session registration and router wiring."
            }
            Self::ApiThreadInfo => {
                "HTTP /send round-trip for a broadcast -thread-info command, exercising router fanout and tracker aggregation."
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
        }
    }

    pub fn metric(&self) -> &'static str {
        match self {
            Self::Startup => "cold_start_ms",
            Self::ApiThreadInfo
            | Self::ApiListGroups
            | Self::CliThreadInfo
            | Self::CliBreakInsert
            | Self::Notifications => "latency_ms",
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
}

#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub scenario: String,
    pub description: &'static str,
    pub metric: &'static str,
    pub sessions: usize,
    pub threads_per_session: usize,
    pub notification_subscribers: Option<usize>,
    pub stats: SummaryStats,
}

pub fn run_scenario(
    kind: ScenarioKind,
    sessions: usize,
    config: &ScenarioConfig<'_>,
) -> Result<ScenarioResult> {
    let stats = match kind {
        ScenarioKind::Startup => SummaryStats::from_durations(&measure_startup(sessions, config)?),
        ScenarioKind::ApiThreadInfo => SummaryStats::from_durations(&measure_with_live_harness(
            sessions,
            config,
            |harness, _, _| {
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
                if responses.len() != sessions {
                    bail!(
                        "expected {} session responses for api-thread-info, got {}",
                        sessions,
                        responses.len()
                    );
                }
                Ok(start.elapsed())
            },
        )?),
        ScenarioKind::ApiListGroups => SummaryStats::from_durations(&measure_with_live_harness(
            sessions,
            config,
            |harness, _, _| {
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
                if responses.len() != sessions {
                    bail!(
                        "expected {} session responses for api-list-groups, got {}",
                        sessions,
                        responses.len()
                    );
                }
                Ok(start.elapsed())
            },
        )?),
        ScenarioKind::CliThreadInfo => SummaryStats::from_durations(&measure_with_live_harness(
            sessions,
            config,
            |harness, iteration, sample_idx| {
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
            },
        )?),
        ScenarioKind::CliBreakInsert => SummaryStats::from_durations(&measure_with_live_harness(
            sessions,
            config,
            |harness, iteration, _| {
                let gid = harness.first_group_id()?;
                let token = 20_000_u64 + iteration as u64;
                let line_number = 1_000_u64 + iteration as u64;
                let cursor = harness.send_cli_cmd(&format!(
                    "{token}-break-insert bench_{sessions}_{iteration}.rs:{line_number} --group {gid}"
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
            },
        )?),
        ScenarioKind::Notifications => SummaryStats::from_durations(&measure_with_live_harness(
            sessions,
            config,
            |harness, iteration, _| {
                let subscribers = harness.connect_notification_subscribers(
                    config.notification_subscribers,
                    config.timeout,
                )?;
                harness.wait_for_notification_subscribers(
                    config.notification_subscribers,
                    config.timeout,
                )?;

                let message = format!("bench-notification-{sessions}-{iteration}");
                let start = Instant::now();
                let response = harness.post_test_notification(&message)?;
                if response["success"].as_bool() != Some(true) {
                    bail!("notification test endpoint returned unsuccessful response");
                }
                subscribers.wait_for_all(&message, config.timeout)?;
                let elapsed = start.elapsed();

                harness.wait_for_notification_subscribers(0, config.timeout)?;
                Ok(elapsed)
            },
        )?),
    };

    Ok(ScenarioResult {
        scenario: kind.as_str().to_string(),
        description: kind.description(),
        metric: kind.metric(),
        sessions,
        threads_per_session: config.threads_per_session,
        notification_subscribers: matches!(kind, ScenarioKind::Notifications)
            .then_some(config.notification_subscribers),
        stats,
    })
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
