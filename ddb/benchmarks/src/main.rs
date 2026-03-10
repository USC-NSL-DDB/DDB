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

#[derive(Debug, Clone, Copy, ValueEnum)]
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
            ScenarioKind::ApiListGroups,
            ScenarioKind::CliThreadInfo,
            ScenarioKind::CliBreakInsert,
            ScenarioKind::Notifications,
        ]
    )]
    scenarios: Vec<ScenarioKind>,

    #[arg(long, value_delimiter = ',', default_values_t = [1_usize, 4, 16, 64])]
    scales: Vec<usize>,

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

    #[arg(long, default_value_t = 12)]
    samples: usize,

    #[arg(long, default_value_t = 2)]
    warmup: usize,

    #[arg(long, default_value_t = 4)]
    startup_samples: usize,

    #[arg(long, default_value_t = 1)]
    startup_warmup: usize,

    #[arg(long, default_value_t = DEFAULT_TIMEOUT_MS)]
    timeout_ms: u64,

    #[arg(long)]
    binary: Option<PathBuf>,

    #[arg(long, default_value_t = OutputFormat::Table)]
    format: OutputFormat,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    generated_at_epoch_s: u64,
    binary: String,
    scales: Vec<usize>,
    dbt_depths: Vec<usize>,
    threads_per_session: usize,
    notification_subscribers: usize,
    warmup: usize,
    samples: usize,
    startup_warmup: usize,
    startup_samples: usize,
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
        notification_subscribers: args.notification_subscribers,
        warmup: args.warmup,
        samples: args.samples,
        startup_warmup: args.startup_warmup,
        startup_samples: args.startup_samples,
    };

    let mut results = Vec::new();
    for scenario in &args.scenarios {
        let scales = if matches!(scenario, ScenarioKind::DistributedBacktrace) {
            &args.dbt_depths
        } else {
            &args.scales
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
        dbt_depths: args.dbt_depths.clone(),
        threads_per_session: args.threads_per_session,
        notification_subscribers: args.notification_subscribers,
        warmup: args.warmup,
        samples: args.samples,
        startup_warmup: args.startup_warmup,
        startup_samples: args.startup_samples,
        results,
    };

    match args.format {
        OutputFormat::Table => print_table(&report),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&report).context("failed to serialize json report")?
        ),
    }

    Ok(())
}

fn validate_args(args: &mut Args) -> Result<()> {
    if args.scenarios.is_empty() {
        bail!("at least one scenario must be selected");
    }
    if args.scales.is_empty() {
        bail!("at least one session scale must be provided");
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
        "binary: {}\nthreads/session: {}\nnotification subscribers: {}\ndbt depths: {:?}\n",
        report.binary,
        report.threads_per_session,
        report.notification_subscribers,
        report.dbt_depths
    );
    println!(
        "{:<24} {:>8} {:>7} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "scenario",
        "sessions",
        "depth",
        "samples",
        "p50 ms",
        "p95 ms",
        "p99 ms",
        "mean ms",
        "max ms"
    );
    println!("{}", "-".repeat(112));

    for result in &report.results {
        println!(
            "{:<24} {:>8} {:>7} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
            result.scenario,
            result.sessions,
            result
                .dbt_depth
                .map(|depth| depth.to_string())
                .unwrap_or_else(|| "-".to_string()),
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
