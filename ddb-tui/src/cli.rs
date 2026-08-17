use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "ddb-tui",
    version,
    about = "Interactive terminal debugger frontend for DDB"
)]
pub(crate) struct Args {
    /// DDB configuration to run under a managed local backend.
    #[arg(value_name = "conf_file", value_parser = parse_existing_path)]
    config: Option<PathBuf>,

    /// Explicit form of the managed DDB configuration path.
    #[arg(long = "config", value_name = "PATH", value_parser = parse_existing_path)]
    config_flag: Option<PathBuf>,

    /// Legacy explicit-connect URL. Prefer `connect --api URL` for new scripts.
    #[arg(long)]
    api: Option<String>,

    /// Bearer token for authenticated external DDB reads and controls.
    #[arg(long, global = true, env = "DDB_API_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// DDB executable used by managed config/launch/attach modes.
    #[arg(long, global = true, value_name = "PATH", value_parser = parse_existing_path)]
    pub ddb_path: Option<PathBuf>,

    /// Maximum seconds to wait for a managed DDB service to become ready.
    #[arg(
        long = "startup-timeout",
        visible_alias = "startup-timeout-secs",
        global = true,
        default_value_t = 20
    )]
    startup_timeout_secs: u64,

    /// Create a persistent backend stdout/stderr log at a new path.
    #[arg(long, global = true, value_name = "PATH")]
    pub backend_log: Option<PathBuf>,

    /// API negotiation policy. Fallback tries v2 first and uses v1 only when
    /// the server explicitly reports that the v2 route is absent.
    #[arg(long, global = true, value_enum, default_value_t = ApiVersion::V2)]
    pub api_version: ApiVersion,

    /// Recovery refresh interval in milliseconds.
    #[arg(long, global = true, default_value_t = 2_000)]
    pub refresh_ms: u64,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Connect to a DDB instance whose lifecycle is externally owned.
    Connect {
        /// Base URL of DDB's HTTP API.
        #[arg(long, required = true)]
        api: String,
    },
    /// Launch a native executable using a managed DDB backend.
    Launch {
        /// Debugger backend used for this session.
        #[arg(long, value_enum, default_value_t = DebuggerBackend::Gdb)]
        backend: DebuggerBackend,

        /// Stop at entry (the default).
        #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_stop_at_entry")]
        stop_at_entry: bool,

        /// Start immediately instead of stopping at entry.
        #[arg(long, action = ArgAction::SetTrue)]
        no_stop_at_entry: bool,

        /// Executable followed by its exact arguments. `--` is required.
        #[arg(required = true, last = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Attach to an existing local process using a managed DDB backend.
    Attach {
        /// Debugger backend used for this session.
        #[arg(long, value_enum, default_value_t = DebuggerBackend::Gdb)]
        backend: DebuggerBackend,

        /// Operating-system process identifier to attach.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..), required = true)]
        pid: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ApiVersion {
    V2,
    V1Fallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum DebuggerBackend {
    Gdb,
    Lldb,
}

impl DebuggerBackend {
    pub(crate) fn as_arg(self) -> &'static str {
        match self {
            Self::Gdb => "gdb",
            Self::Lldb => "lldb",
        }
    }
}

#[derive(Debug)]
pub(crate) enum Mode {
    ManagedConfig(PathBuf),
    ManagedLaunch {
        backend: DebuggerBackend,
        stop_at_entry: bool,
        command: Vec<OsString>,
    },
    ManagedAttach {
        backend: DebuggerBackend,
        pid: u64,
    },
    Connect {
        api: String,
    },
}

impl Args {
    pub(crate) fn parse_mode(&self) -> Result<Mode> {
        if self.config.is_some() && self.config_flag.is_some() {
            anyhow::bail!("the positional config and --config cannot be used together");
        }
        let config = self.config.clone().or_else(|| self.config_flag.clone());
        if config.is_some() && self.api.is_some() && self.command.is_none() {
            anyhow::bail!("configuration and --api select conflicting ownership modes");
        }

        match &self.command {
            Some(Command::Connect { api }) => {
                reject_managed_root_options(config.as_deref(), self.api.as_deref())?;
                Ok(Mode::Connect { api: api.clone() })
            }
            Some(Command::Launch {
                backend,
                stop_at_entry,
                no_stop_at_entry,
                command,
            }) => {
                reject_managed_root_options(config.as_deref(), self.api.as_deref())?;
                Ok(Mode::ManagedLaunch {
                    backend: *backend,
                    stop_at_entry: !*no_stop_at_entry || *stop_at_entry,
                    command: command.clone(),
                })
            }
            Some(Command::Attach { backend, pid }) => {
                reject_managed_root_options(config.as_deref(), self.api.as_deref())?;
                Ok(Mode::ManagedAttach {
                    backend: *backend,
                    pid: *pid,
                })
            }
            None => match (config, &self.api) {
                (Some(config), None) => Ok(Mode::ManagedConfig(config)),
                (None, Some(api)) => Ok(Mode::Connect { api: api.clone() }),
                (None, None) => Ok(Mode::Connect {
                    api: "http://127.0.0.1:5000".to_string(),
                }),
                (Some(_), Some(_)) => unreachable!("conflict handled below"),
            },
        }
    }

    pub(crate) fn startup_timeout(&self) -> Duration {
        Duration::from_secs(self.startup_timeout_secs.max(1))
    }
}

fn reject_managed_root_options(config: Option<&Path>, legacy_api: Option<&str>) -> Result<()> {
    if config.is_some() {
        anyhow::bail!("a configuration path cannot be combined with a TUI subcommand");
    }
    if legacy_api.is_some() {
        anyhow::bail!("root --api cannot be combined with a TUI subcommand");
    }
    Ok(())
}

fn parse_existing_path(path: &str) -> Result<PathBuf, io::Error> {
    PathBuf::from(path).canonicalize()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn positional_config_selects_managed_mode() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("ddb.yaml");
        fs::write(&config, "Framework: unspecified\n").unwrap();
        let args = Args::try_parse_from(["ddb-tui", config.to_str().unwrap()]).unwrap();
        assert!(
            matches!(args.parse_mode().unwrap(), Mode::ManagedConfig(path) if path == config.canonicalize().unwrap())
        );
    }

    #[test]
    fn legacy_api_form_stays_external_connect() {
        let args = Args::try_parse_from(["ddb-tui", "--api", "http://127.0.0.1:7000"]).unwrap();
        assert!(matches!(
            args.parse_mode().unwrap(),
            Mode::Connect { api } if api == "http://127.0.0.1:7000"
        ));
    }

    #[test]
    fn launch_preserves_argument_boundaries() {
        let args = Args::try_parse_from([
            "ddb-tui",
            "launch",
            "--backend",
            "lldb",
            "--",
            "./app",
            "--flag",
            "value with spaces",
        ])
        .unwrap();
        let Mode::ManagedLaunch {
            backend,
            stop_at_entry,
            command,
        } = args.parse_mode().unwrap()
        else {
            panic!("launch mode expected");
        };
        assert_eq!(backend, DebuggerBackend::Lldb);
        assert!(stop_at_entry);
        assert_eq!(
            command,
            ["./app", "--flag", "value with spaces"].map(OsString::from)
        );
    }

    #[test]
    fn attach_rejects_zero_pid() {
        Args::try_parse_from(["ddb-tui", "attach", "--pid", "0"])
            .expect_err("zero is never a valid attach PID");
    }

    #[test]
    fn config_and_connect_are_unambiguously_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("ddb.yaml");
        fs::write(&config, "Framework: unspecified\n").unwrap();
        let args = Args::try_parse_from([
            "ddb-tui",
            config.to_str().unwrap(),
            "--api",
            "http://127.0.0.1:5000",
        ])
        .unwrap();
        assert!(args
            .parse_mode()
            .unwrap_err()
            .to_string()
            .contains("conflict"));
    }

    #[test]
    fn startup_timeout_has_a_stable_name_and_seconds_alias() {
        let canonical = Args::try_parse_from(["ddb-tui", "--startup-timeout", "7"]).unwrap();
        assert_eq!(canonical.startup_timeout(), Duration::from_secs(7));

        let alias = Args::try_parse_from(["ddb-tui", "--startup-timeout-secs", "9"]).unwrap();
        assert_eq!(alias.startup_timeout(), Duration::from_secs(9));
    }
}
