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
    #[arg(long, default_value = "http://68.181.216.50:54317")]
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

    /// Number of workers used to execute commands and collect their responses
    #[arg(long, default_value_t = 10, value_parser = parse_positive_usize)]
    pub command_workers: usize,
}

fn parse_path(path: &str) -> Result<PathBuf, io::Error> {
    PathBuf::from(path).canonicalize()
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid positive integer: {value}"))?;
    if parsed == 0 {
        return Err("value must be greater than zero".to_string());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::Parser;

    #[test]
    fn command_workers_default_to_ten() {
        let args = Args::try_parse_from(["ddb"]).unwrap();
        assert_eq!(args.command_workers, 10);
    }

    #[test]
    fn command_workers_are_configurable() {
        let args = Args::try_parse_from(["ddb", "--command-workers", "64"]).unwrap();
        assert_eq!(args.command_workers, 64);
    }

    #[test]
    fn command_workers_must_be_positive() {
        assert!(Args::try_parse_from(["ddb", "--command-workers", "0"]).is_err());
    }
}
