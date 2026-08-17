use std::{
    ffi::OsString,
    io,
    net::IpAddr,
    path::{Path, PathBuf},
};

use clap::{ArgAction, Args as ClapArgs, Parser, Subcommand, ValueEnum};

/// Distributed debugger backend and frontend launcher.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path of the debugging config file. This legacy/default form keeps the
    /// interactive stdin command loop enabled.
    #[arg(value_name = "conf_file", value_parser = parse_path)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub logging: LoggingArgs,

    /// Number of workers used to execute commands and collect their responses.
    #[arg(long, global = true, default_value_t = 10, value_parser = parse_positive_usize)]
    pub command_workers: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            config: None,
            command: None,
            logging: LoggingArgs::default(),
            command_workers: 10,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Run DDB as an API service without the stdin command loop.
    Serve(ServeArgs),
    /// Start the companion ddb-tui frontend.
    #[command(disable_help_flag = true, disable_version_flag = true)]
    Tui(TuiArgs),
}

#[derive(ClapArgs, Debug, Clone, Default)]
pub struct TuiArgs {
    /// Arguments passed verbatim to ddb-tui.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

#[derive(ClapArgs, Debug, Clone)]
pub struct ServeArgs {
    /// Path of the debugging config file.
    #[arg(value_name = "conf_file", value_parser = parse_path)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub session: Option<ServeSession>,

    /// Override the configured HTTP API bind address.
    #[arg(long)]
    pub api_bind: Option<IpAddr>,

    /// Override the configured HTTP API port. Port 0 asks the OS to allocate one.
    #[arg(long)]
    pub api_port: Option<u16>,

    /// Override the configured bearer-token document path.
    #[arg(long, value_name = "PATH", value_parser = parse_path)]
    pub api_auth_token_file: Option<PathBuf>,

    /// Write a bounded machine-readable startup result atomically to this path.
    #[arg(long, value_name = "PATH")]
    pub startup_report: Option<PathBuf>,

    /// Enforce the secure loopback/ephemeral transport policy used by ddb-tui.
    #[arg(long, hide = true)]
    pub managed: bool,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ServeSession {
    /// Launch a native executable under the selected debugger.
    Launch(LaunchArgs),
    /// Attach the selected debugger to an existing local process.
    Attach(AttachArgs),
}

#[derive(ClapArgs, Debug, Clone)]
pub struct LaunchArgs {
    /// Debugger backend used for this session.
    #[arg(long, value_enum, default_value_t = BackendArg::Gdb)]
    pub backend: BackendArg,

    /// Stop at the program entry point (the default for interactive launch).
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_stop_at_entry")]
    pub stop_at_entry: bool,

    /// Start immediately instead of stopping at the program entry point.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_stop_at_entry: bool,

    /// Executable followed by its exact argument vector. `--` is required.
    #[arg(required = true, last = true, allow_hyphen_values = true)]
    pub command: Vec<OsString>,
}

impl LaunchArgs {
    pub fn should_stop_at_entry(&self) -> bool {
        !self.no_stop_at_entry || self.stop_at_entry
    }
}

#[derive(ClapArgs, Debug, Clone)]
pub struct AttachArgs {
    /// Debugger backend used for this session.
    #[arg(long, value_enum, default_value_t = BackendArg::Gdb)]
    pub backend: BackendArg,

    /// Operating-system process identifier to attach.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..), required = true)]
    pub pid: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum BackendArg {
    Gdb,
    Lldb,
}

#[derive(ClapArgs, Debug, Clone)]
pub struct LoggingArgs {
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub console_log: bool,

    #[arg(long, global = true, default_value = "info")]
    pub console_level: String,

    #[arg(long, global = true, default_value = "info")]
    pub file_level: String,

    /// OpenTelemetry collector gRPC endpoint.
    #[arg(long, global = true, default_value = "http://68.181.216.50:54317")]
    pub otel_endpoint: String,

    /// OpenTelemetry level.
    #[arg(long, global = true, default_value = "info")]
    pub otel_level: String,

    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub enable_otel: bool,

    #[arg(long, global = true)]
    pub user_id: Option<String>,

    #[arg(long, global = true)]
    pub session_id: Option<String>,
}

impl Default for LoggingArgs {
    fn default() -> Self {
        Self {
            console_log: false,
            console_level: "info".to_string(),
            file_level: "info".to_string(),
            otel_endpoint: "http://68.181.216.50:54317".to_string(),
            otel_level: "info".to_string(),
            enable_otel: false,
            user_id: None,
            session_id: None,
        }
    }
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

pub(crate) fn validate_serve_args(args: &ServeArgs) -> anyhow::Result<()> {
    if args.config.is_some() && args.session.is_some() {
        anyhow::bail!(
            "serve accepts either a configuration file or a launch/attach command, not both"
        );
    }
    if args.config.is_none() && args.session.is_none() {
        anyhow::bail!("serve requires a configuration file or a launch/attach command");
    }
    if args.managed {
        if args.api_bind.is_some_and(|bind| !bind.is_loopback()) {
            anyhow::bail!("managed serve mode only permits a loopback API bind");
        }
        if args.api_port.is_some_and(|port| port != 0) {
            anyhow::bail!("managed serve mode requires OS-assigned API port 0");
        }
        if args.api_auth_token_file.is_none() {
            anyhow::bail!("managed serve mode requires --api-auth-token-file");
        }
        if args.startup_report.is_none() {
            anyhow::bail!("managed serve mode requires --startup-report");
        }
    }
    if let Some(report) = &args.startup_report {
        let parent = report.parent().unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            anyhow::bail!(
                "startup report parent directory {} does not exist",
                parent.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;

    use super::*;

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

    #[test]
    fn parse_path_canonicalizes_existing_paths() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let file = dir.path().join("ddb.yaml");
        fs::write(&file, "Framework: nu\n").expect("temp file should be written");

        let parsed = parse_path(file.to_str().expect("path should be valid utf-8"))
            .expect("existing path should parse");

        assert_eq!(
            parsed,
            file.canonicalize().expect("file should canonicalize")
        );
    }

    #[test]
    fn parse_path_rejects_missing_paths() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let file = dir.path().join("missing.yaml");

        assert!(parse_path(file.to_str().expect("path should be valid utf-8")).is_err());
    }

    #[test]
    fn legacy_positional_config_and_logging_flags_remain_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("ddb.yaml");
        fs::write(&config, "Framework: unspecified\n").unwrap();
        let args = Args::try_parse_from([
            "ddb",
            config.to_str().unwrap(),
            "--console-log",
            "--console-level",
            "debug",
        ])
        .unwrap();
        assert_eq!(
            args.config.as_deref(),
            Some(config.canonicalize().unwrap().as_path())
        );
        assert!(args.logging.console_log);
        assert_eq!(args.logging.console_level, "debug");
        assert!(args.command.is_none());
    }

    #[test]
    fn tui_arguments_are_preserved_verbatim() {
        let args = Args::try_parse_from([
            "ddb",
            "tui",
            "launch",
            "--backend",
            "gdb",
            "--",
            "./program",
            "--flag",
        ])
        .unwrap();
        let Some(Command::Tui(tui)) = args.command else {
            panic!("tui command should parse");
        };
        assert_eq!(
            tui.args,
            ["launch", "--backend", "gdb", "--", "./program", "--flag"].map(OsString::from)
        );
    }

    #[test]
    fn managed_serve_requires_private_coordination_inputs() {
        let args =
            Args::try_parse_from(["ddb", "serve", "--managed", "launch", "--", "app"]).unwrap();
        let Some(Command::Serve(serve)) = args.command else {
            panic!("serve command should parse");
        };
        let error = validate_serve_args(&serve).unwrap_err();
        assert!(error.to_string().contains("api-auth-token-file"));
    }

    #[test]
    fn launch_requires_the_debuggee_separator() {
        let error = Args::try_parse_from(["ddb", "serve", "launch", "program"])
            .expect_err("launch without -- must be rejected");
        assert!(error.to_string().contains("-- <COMMAND>..."));
    }
}
