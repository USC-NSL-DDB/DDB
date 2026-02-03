use clap::Parser;
use std::io;
use std::path::PathBuf;

/// Interactive debugger for distributed software
#[derive(Parser, Debug, Default, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path of the debugging config file
    #[arg(value_name = "conf_file", value_parser=parse_path)]
    pub config: Option<PathBuf>,
    // /// Enable debug mode
    // #[arg(long)]
    // pub debug: bool,
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub console_log: bool,

    #[arg(long, default_value = "info")]
    pub console_level: String,

    #[arg(long, default_value = "info")]
    pub file_level: String,

    /// OpenTelemetry collector gRPC endpoint
    #[arg(long, default_value = "http://ruby1.nsl.usc.edu:54317")]
    pub otel_endpoint: String,

    /// OpenTelemetry level
    #[arg(long, default_value = "info")]
    pub otel_level: String,

    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub enable_otel: bool,

    #[arg(long, default_value = None)]
    pub user_id: Option<String>,

    #[arg(long, default_value = None)]
    pub session_id: Option<String>,
}

fn parse_path(path: &str) -> Result<PathBuf, io::Error> {
    PathBuf::from(path).canonicalize()
}
