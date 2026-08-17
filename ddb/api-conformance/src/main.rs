use std::{process::ExitCode, time::Duration};

use anyhow::Result;
use clap::{Parser, ValueEnum};
use ddb_api_conformance::{run, CheckStatus, ConformanceOptions, ConformanceProfile};

#[derive(Debug, Parser)]
#[command(
    name = "ddb-api-conformance",
    about = "Validate a DDB API v2 server through its public SDK boundary"
)]
struct Args {
    /// DDB HTTP endpoint, including an optional deployment path prefix.
    #[arg(
        long,
        env = "DDB_API_ENDPOINT",
        default_value = "http://127.0.0.1:5000"
    )]
    endpoint: String,

    /// Bearer credential. Prefer DDB_API_TOKEN to avoid shell history.
    #[arg(long, env = "DDB_API_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Safe read-only checks or the mutating deterministic Mock fixture profile.
    #[arg(long, value_enum, default_value_t = ProfileArg::ReadOnly)]
    profile: ProfileArg,

    /// Maximum number of items collected from any public collection.
    #[arg(long, default_value_t = 10_000)]
    max_collection_items: usize,

    /// Unary request deadline in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    request_timeout_ms: u64,

    /// Stream connection/event deadline in milliseconds.
    #[arg(long, default_value_t = 5_000)]
    stream_timeout_ms: u64,

    /// Report rendering.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output: OutputFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArg {
    ReadOnly,
    Mock,
}

impl From<ProfileArg> for ConformanceProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::ReadOnly => Self::ReadOnly,
            ProfileArg::Mock => Self::Mock,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = Args::parse();
    let report = run(ConformanceOptions {
        endpoint: args.endpoint,
        bearer_token: args.token,
        profile: args.profile.into(),
        max_collection_items: args.max_collection_items,
        request_timeout: Duration::from_millis(args.request_timeout_ms),
        stream_timeout: Duration::from_millis(args.stream_timeout_ms),
        ..Default::default()
    })
    .await?;

    match args.output {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            println!(
                "DDB API conformance: {} ({} passed, {} failed, {} skipped)",
                if report.passed() { "PASS" } else { "FAIL" },
                report.passed_count(),
                report.failed_count(),
                report.skipped_count()
            );
            println!(
                "endpoint={} profile={} api={}/{} server={}",
                report.endpoint,
                report.profile,
                report.api_version.as_deref().unwrap_or("unknown"),
                report.schema_version.as_deref().unwrap_or("unknown"),
                report.server_version.as_deref().unwrap_or("unknown")
            );
            for check in &report.checks {
                let marker = match check.status {
                    CheckStatus::Passed => "PASS",
                    CheckStatus::Failed => "FAIL",
                    CheckStatus::Skipped => "SKIP",
                };
                println!(
                    "[{marker}] {} ({} ms): {}",
                    check.name, check.duration_millis, check.detail
                );
            }
        }
    }

    Ok(if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}
