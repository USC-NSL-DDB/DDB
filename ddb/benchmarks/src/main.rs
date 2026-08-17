mod harness;
mod scenario;
mod stats;

use std::{
    fmt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use scenario::{run_scenario, ScenarioConfig, ScenarioKind, ScenarioResult};
use serde::Serialize;

const DEFAULT_NOTIFICATION_SUBSCRIBERS: usize = 8;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_BULK_OUTPUT_EVENTS: usize = 2_048;
const DEFAULT_BULK_OUTPUT_EVENT_BYTES: usize = 4_096;
const DEFAULT_VARIABLES_PER_FRAME: usize = 500;
const DEFAULT_MEMORY_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_INSPECTION_VARIABLES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Table => f.write_str("table"),
            Self::Json => f.write_str("json"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "ddb-bench",
    about = "Benchmark the DDB debugger using generated mock-session topologies."
)]
struct Args {
    #[arg(
        long,
        value_delimiter = ',',
        default_values_t = [
            ScenarioKind::Startup,
            ScenarioKind::ApiThreadInfo,
            ScenarioKind::ApiThreadInfoBurst,
            ScenarioKind::ApiListGroups,
            ScenarioKind::V2HttpSnapshot,
            ScenarioKind::V2GrpcSnapshot,
            ScenarioKind::V2HttpStepStop,
            ScenarioKind::V2GrpcStepStop,
            ScenarioKind::CliThreadInfo,
            ScenarioKind::CliBreakInsert,
            ScenarioKind::Notifications,
        ]
    )]
    scenarios: Vec<ScenarioKind>,

    #[arg(long, value_delimiter = ',', default_values_t = [1_usize, 4, 16, 64])]
    scales: Vec<usize>,

    /// Total variables projected per large-inspection sample (1..=1000000).
    #[arg(long, value_delimiter = ',', default_values_t = [10_000_usize])]
    inspection_variables: Vec<usize>,

    /// Deterministic variables returned per bounded Mock frame query (1..=500).
    #[arg(long, default_value_t = DEFAULT_VARIABLES_PER_FRAME)]
    variables_per_frame: usize,

    /// Total MiB read through bounded public ReadMemory chunks.
    #[arg(long, value_delimiter = ',', default_values_t = [1_usize, 16, 64])]
    memory_sizes_mib: Vec<usize>,

    /// Bytes requested per ReadMemory call (1..=1048576).
    #[arg(long, default_value_t = DEFAULT_MEMORY_CHUNK_BYTES)]
    memory_chunk_bytes: usize,

    /// Concurrent public state subscribers used by the state-fanout scenario.
    #[arg(long, value_delimiter = ',', default_values_t = [1_usize, 8, 20])]
    state_subscribers: Vec<usize>,

    #[arg(
        long,
        value_delimiter = ',',
        default_values_t = [1_usize, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
    )]
    dbt_depths: Vec<usize>,

    #[arg(long, default_value_t = 4)]
    threads_per_session: usize,

    #[arg(long, default_value_t = DEFAULT_NOTIFICATION_SUBSCRIBERS)]
    notification_subscribers: usize,

    /// Output envelopes generated per mixed-output sample (1..=4096).
    #[arg(long, default_value_t = DEFAULT_BULK_OUTPUT_EVENTS)]
    bulk_output_events: usize,

    /// UTF-8 bytes generated in each mixed-output envelope (1..=65536).
    #[arg(long, default_value_t = DEFAULT_BULK_OUTPUT_EVENT_BYTES)]
    bulk_output_event_bytes: usize,

    #[arg(long, default_value_t = 12)]
    samples: usize,

    #[arg(long, default_value_t = 2)]
    warmup: usize,

    #[arg(long, default_value_t = 4)]
    startup_samples: usize,

    #[arg(long, default_value_t = 1)]
    startup_warmup: usize,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    lldb_eager_stack_warmup: bool,

    #[arg(long, default_value_t = DEFAULT_TIMEOUT_MS)]
    timeout_ms: u64,

    #[arg(long)]
    binary: Option<PathBuf>,

    #[arg(long, default_value_t = OutputFormat::Table)]
    format: OutputFormat,

    /// Write JSON evidence to this file instead of stdout. Requires --format json.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    generated_at_epoch_s: u64,
    binary: String,
    scales: Vec<usize>,
    inspection_variables: Vec<usize>,
    variables_per_frame: usize,
    memory_sizes_mib: Vec<usize>,
    memory_chunk_bytes: usize,
    state_subscribers: Vec<usize>,
    dbt_depths: Vec<usize>,
    threads_per_session: usize,
    notification_subscribers: usize,
    bulk_output_events: usize,
    bulk_output_event_bytes: usize,
    bulk_output_total_bytes: u64,
    warmup: usize,
    samples: usize,
    startup_warmup: usize,
    startup_samples: usize,
    lldb_eager_stack_warmup: bool,
    results: Vec<ScenarioResult>,
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    validate_args(&mut args)?;

    let workspace_root = workspace_root();
    let binary = resolve_ddb_binary(&args, &workspace_root)?;
    let timeout = Duration::from_millis(args.timeout_ms);
    let config = ScenarioConfig {
        binary: &binary,
        workspace_root: &workspace_root,
        timeout,
        threads_per_session: args.threads_per_session,
        variables_per_frame: args.variables_per_frame,
        memory_chunk_bytes: args.memory_chunk_bytes,
        notification_subscribers: args.notification_subscribers,
        warmup: args.warmup,
        samples: args.samples,
        startup_warmup: args.startup_warmup,
        startup_samples: args.startup_samples,
        lldb_eager_stack_warmup: args.lldb_eager_stack_warmup,
        bulk_output_events: args.bulk_output_events,
        bulk_output_event_bytes: args.bulk_output_event_bytes,
    };

    let mut results = Vec::new();
    for scenario in &args.scenarios {
        let scales = match scenario {
            ScenarioKind::DistributedBacktrace | ScenarioKind::LldbDistributedBacktrace => {
                &args.dbt_depths
            }
            ScenarioKind::V2HttpVariableInspection => &args.inspection_variables,
            ScenarioKind::V2HttpMemoryTransfer => &args.memory_sizes_mib,
            ScenarioKind::V2HttpStateFanout => &args.state_subscribers,
            _ => &args.scales,
        };
        for &sessions in scales {
            eprintln!("running {} with scale {}...", scenario.as_str(), sessions);
            results.push(run_scenario(*scenario, sessions, &config)?);
        }
    }

    let report = BenchmarkReport {
        generated_at_epoch_s: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_secs(),
        binary: binary.display().to_string(),
        scales: args.scales.clone(),
        inspection_variables: args.inspection_variables.clone(),
        variables_per_frame: args.variables_per_frame,
        memory_sizes_mib: args.memory_sizes_mib.clone(),
        memory_chunk_bytes: args.memory_chunk_bytes,
        state_subscribers: args.state_subscribers.clone(),
        dbt_depths: args.dbt_depths.clone(),
        threads_per_session: args.threads_per_session,
        notification_subscribers: args.notification_subscribers,
        bulk_output_events: args.bulk_output_events,
        bulk_output_event_bytes: args.bulk_output_event_bytes,
        bulk_output_total_bytes: (args.bulk_output_events as u64)
            .saturating_mul(args.bulk_output_event_bytes as u64),
        warmup: args.warmup,
        samples: args.samples,
        startup_warmup: args.startup_warmup,
        startup_samples: args.startup_samples,
        lldb_eager_stack_warmup: args.lldb_eager_stack_warmup,
        results,
    };

    match args.format {
        OutputFormat::Table => print_table(&report),
        OutputFormat::Json => {
            let json =
                serde_json::to_string_pretty(&report).context("failed to serialize json report")?;
            if let Some(path) = args.output.as_ref() {
                std::fs::write(path, format!("{json}\n")).with_context(|| {
                    format!("failed to write benchmark report {}", path.display())
                })?;
            } else {
                println!("{json}");
            }
        }
    }

    Ok(())
}

fn validate_args(args: &mut Args) -> Result<()> {
    if args.output.is_some() && args.format != OutputFormat::Json {
        bail!("--output requires --format json");
    }
    if args.scenarios.is_empty() {
        bail!("at least one scenario must be selected");
    }
    if args.scales.is_empty() {
        bail!("at least one session scale must be provided");
    }
    if args.inspection_variables.is_empty()
        || args
            .inspection_variables
            .iter()
            .any(|count| !(1..=MAX_INSPECTION_VARIABLES).contains(count))
    {
        bail!("inspection-variables must be in the range 1..=1000000");
    }
    if !(1..=500).contains(&args.variables_per_frame) {
        bail!("variables-per-frame must be in the range 1..=500");
    }
    if args.memory_sizes_mib.is_empty()
        || args
            .memory_sizes_mib
            .iter()
            .any(|size| !(1..=1024).contains(size))
    {
        bail!("memory-sizes-mib must be in the range 1..=1024");
    }
    if !(1..=1024 * 1024).contains(&args.memory_chunk_bytes) {
        bail!("memory-chunk-bytes must be in the range 1..=1048576");
    }
    if args.state_subscribers.is_empty()
        || args
            .state_subscribers
            .iter()
            .any(|count| !(1..=20).contains(count))
    {
        bail!("state-subscribers must be in the range 1..=20");
    }
    if args.dbt_depths.is_empty() {
        bail!("at least one distributed-backtrace depth must be provided");
    }
    if args.threads_per_session == 0 {
        bail!("threads-per-session must be greater than zero");
    }
    if args.notification_subscribers == 0 {
        bail!("notification-subscribers must be greater than zero");
    }
    if args.notification_subscribers > 20 {
        bail!("notification-subscribers cannot exceed the current manager limit of 20");
    }
    if !(1..=4_096).contains(&args.bulk_output_events) {
        bail!("bulk-output-events must be in the range 1..=4096");
    }
    if !(1..=65_536).contains(&args.bulk_output_event_bytes) {
        bail!("bulk-output-event-bytes must be in the range 1..=65536");
    }
    if args.samples == 0 || args.startup_samples == 0 {
        bail!("sample counts must be greater than zero");
    }
    if args
        .dbt_depths
        .iter()
        .any(|depth| *depth == 0 || *depth > 16)
    {
        bail!("distributed-backtrace depths must be in the range 1..=16");
    }

    args.scales.sort_unstable();
    args.scales.dedup();
    args.inspection_variables.sort_unstable();
    args.inspection_variables.dedup();
    args.memory_sizes_mib.sort_unstable();
    args.memory_sizes_mib.dedup();
    args.state_subscribers.sort_unstable();
    args.state_subscribers.dedup();
    args.dbt_depths.sort_unstable();
    args.dbt_depths.dedup();
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("benchmark crate should live directly under the workspace")
        .to_path_buf()
}

fn resolve_ddb_binary(args: &Args, workspace_root: &Path) -> Result<PathBuf> {
    if let Some(binary) = &args.binary {
        return binary
            .canonicalize()
            .with_context(|| format!("failed to resolve benchmark binary {}", binary.display()));
    }

    let binary_name = if cfg!(windows) { "ddb.exe" } else { "ddb" };
    let binary = workspace_root
        .join("target")
        .join("release")
        .join(binary_name);
    eprintln!("building {} benchmark target...", binary_name);
    let status = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("-p")
        .arg("ddb")
        .arg("--release")
        .arg("--features")
        .arg("grpc-preview")
        .current_dir(workspace_root)
        .status()
        .context("failed to invoke cargo build for ddb")?;
    if !status.success() {
        bail!("cargo build -p ddb --release failed");
    }

    binary
        .canonicalize()
        .with_context(|| format!("failed to resolve benchmark binary {}", binary.display()))
}

fn print_table(report: &BenchmarkReport) {
    println!(
        "binary: {}\nthreads/session: {}\ninspection variables: {:?} ({} per frame)\nmemory sizes MiB: {:?} ({} byte chunks)\nstate subscribers: {:?}\nnotification subscribers: {}\nbulk output: {} x {} bytes ({} bytes total)\ndbt depths: {:?}\n",
        report.binary,
        report.threads_per_session,
        report.inspection_variables,
        report.variables_per_frame,
        report.memory_sizes_mib,
        report.memory_chunk_bytes,
        report.state_subscribers,
        report.notification_subscribers,
        report.bulk_output_events,
        report.bulk_output_event_bytes,
        report.bulk_output_total_bytes,
        report.dbt_depths
    );
    println!(
        "{:<32} {:>8} {:>12} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "scenario", "scale", "unit", "samples", "p50 ms", "p95 ms", "p99 ms", "mean ms", "max ms"
    );
    println!("{}", "-".repeat(128));

    for result in &report.results {
        println!(
            "{:<32} {:>8} {:>12} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
            result.scenario,
            result.scale,
            result.scale_unit,
            result.stats.samples,
            fmt_ms(result.stats.p50_ms),
            fmt_ms(result.stats.p95_ms),
            fmt_ms(result.stats.p99_ms),
            fmt_ms(result.stats.mean_ms),
            fmt_ms(result.stats.max_ms),
        );
    }
}

fn fmt_ms(value: f64) -> String {
    format!("{value:.2}")
}
