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

    /// Additional managed-backend option, e.g. --ddb-arg=--command-workers=4.
    /// Only logging, telemetry and worker options are accepted; never transport.
    #[arg(long = "ddb-arg", global = true, require_equals = true, value_parser = parse_ddb_argument)]
    pub ddb_args: Vec<String>,

    /// Explicit local source roots and debugger-path mappings (YAML).
    #[arg(long, global = true, value_name = "PATH", value_parser = parse_existing_path)]
    pub source_workspace: Option<PathBuf>,

    /// Explicit backend source mappings and GDB/LLDB startup commands (YAML).
    #[arg(long, global = true, value_name = "PATH", value_parser = parse_existing_path)]
    pub debugger_settings: Option<PathBuf>,

    /// Load and automatically save watches in an explicitly trusted JSON file.
    #[arg(long, global = true, value_name = "PATH")]
    pub watch_file: Option<PathBuf>,

    /// Value presentation: enable GDB visualizers, use raw expandable values,
    /// or disable child expansion. Does not disable already-installed formatters.
    #[arg(long, global = true, value_enum, default_value_t = ValuesFormatting::PrettyPrinters)]
    pub values_formatting: ValuesFormatting,

    /// Show distributed callers by default, or explicitly inspect only local frames.
    #[arg(long, global = true, value_enum, default_value_t = StackMode::Distributed)]
    pub stack_mode: StackMode,

    /// Show sanitized API request metadata in Output (no payloads or credentials).
    #[arg(long, global = true)]
    pub trace_api: bool,

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

    /// Recovery refresh interval in milliseconds.
    #[arg(long, global = true, default_value_t = 2_000)]
    pub refresh_ms: u64,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum StackMode {
    #[default]
    Distributed,
    Local,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ValuesFormatting {
    #[default]
    PrettyPrinters,
    Raw,
    Disabled,
}

impl ValuesFormatting {
    pub fn expandable(self) -> bool {
        self != Self::Disabled
    }
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
        if !self.ddb_args.is_empty() {
            if matches!(&self.command, Some(Command::Connect { .. }))
                || (self.command.is_none() && config.is_none())
            {
                anyhow::bail!("--ddb-arg requires a managed config, launch or attach mode");
            }
            anyhow::ensure!(
                self.ddb_args.len() <= 32,
                "at most 32 backend arguments are allowed"
            );
            let mut names = std::collections::HashSet::new();
            for argument in &self.ddb_args {
                let name = argument.split('=').next().unwrap();
                anyhow::ensure!(names.insert(name), "duplicate backend option: {name}");
            }
        }
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

fn parse_ddb_argument(argument: &str) -> Result<String, String> {
    if argument.len() > 4096 || argument.chars().any(char::is_control) {
        return Err("backend arguments must contain no controls and be at most 4096 bytes".into());
    }
    let (name, value) = argument
        .split_once('=')
        .map_or((argument, None), |(name, value)| (name, Some(value)));
    match name {
        "--console-log" | "--enable-otel" if value.is_none() => {},
        "--console-level" | "--file-level" | "--otel-endpoint" | "--otel-level" | "--user-id" | "--session-id"
            if value.is_some_and(|value| !value.trim().is_empty()) => {},
        "--command-workers" if value.and_then(|value| value.parse::<usize>().ok()).is_some_and(|count| count > 0) => {},
        _ => return Err("unsupported backend argument: use a documented logging/telemetry option or --command-workers=N; values require --name=value, and transport/session launch options are managed by ddb-tui".into()),
    }
    Ok(argument.into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn backend_arguments_require_managed_mode_and_reject_overrides() {
        for argument in [
            "--api-bind=0.0.0.0",
            "--api-port=5000",
            "--api-auth-token-file=/tmp/token",
            "--startup-report=/tmp/report",
            "--managed",
            "--",
            "launch",
            "config.yaml",
            "--backend=lldb",
            "--help",
            "--unknown=x",
            "--command-workers=0",
            "--command-workers=-1",
            "--console-level",
            "--console-level=",
            "--console-log=true",
            "--session-id=a\nb",
        ] {
            assert!(parse_ddb_argument(argument).is_err(), "{argument:?}");
        }
        assert!(parse_ddb_argument(&format!("--session-id={}", "x".repeat(4096))).is_err());
        for arguments in [
            vec!["ddb-tui", "--ddb-arg=--command-workers=4"],
            vec![
                "ddb-tui",
                "connect",
                "--api",
                "http://localhost",
                "--ddb-arg=--file-level=debug",
            ],
            vec![
                "ddb-tui",
                "--api",
                "http://localhost",
                "--ddb-arg=--file-level=debug",
            ],
            vec![
                "ddb-tui",
                "--ddb-arg=--command-workers=4",
                "--ddb-arg=--command-workers=8",
                "launch",
                "--",
                "app",
            ],
        ] {
            assert!(Args::try_parse_from(arguments)
                .unwrap()
                .parse_mode()
                .is_err());
        }
        let args = Args::try_parse_from([
            "ddb-tui",
            "--ddb-arg=--command-workers=4",
            "--ddb-arg=--session-id=value with spaces",
            "launch",
            "--",
            "app",
            "--ddb-arg=debuggee-only",
        ])
        .unwrap();
        assert_eq!(
            args.ddb_args,
            ["--command-workers=4", "--session-id=value with spaces"]
        );
        let Mode::ManagedLaunch { command, .. } = args.parse_mode().unwrap() else {
            panic!("expected managed launch")
        };
        assert_eq!(
            command,
            [
                OsString::from("app"),
                OsString::from("--ddb-arg=debuggee-only")
            ]
        );
        for argument in [
            "--console-log",
            "--enable-otel",
            "--console-level=debug",
            "--file-level=warn",
            "--otel-level=info",
            "--otel-endpoint=http://localhost:4317",
            "--user-id=user",
            "--session-id=session",
            "--command-workers=4",
        ] {
            assert_eq!(parse_ddb_argument(argument).unwrap(), argument);
        }
    }
    #[test]
    fn stack_mode_is_explicit_global_and_distributed_by_default() {
        assert!(!Args::try_parse_from(["ddb-tui"]).unwrap().trace_api);
        assert!(
            Args::try_parse_from([
                "ddb-tui",
                "connect",
                "--api",
                "http://localhost",
                "--trace-api"
            ])
            .unwrap()
            .trace_api
        );
        assert_eq!(
            Args::try_parse_from(["ddb-tui"]).unwrap().stack_mode,
            StackMode::Distributed
        );
        for arguments in [
            vec![
                "ddb-tui",
                "--stack-mode",
                "local",
                "connect",
                "--api",
                "http://localhost",
            ],
            vec![
                "ddb-tui",
                "connect",
                "--api",
                "http://localhost",
                "--stack-mode",
                "local",
            ],
            vec![
                "ddb-tui",
                "--stack-mode",
                "local",
                "launch",
                "--",
                "/bin/true",
            ],
        ] {
            let args = Args::try_parse_from(arguments).unwrap();
            assert_eq!(args.stack_mode, StackMode::Local);
            args.parse_mode().unwrap();
        }
        assert!(Args::try_parse_from(["ddb-tui", "--stack-mode", "automatic"]).is_err());
    }
    #[test]
    fn watch_file_is_explicit_global_and_may_be_new() {
        use super::*;
        assert!(Args::try_parse_from(["ddb-tui"])
            .unwrap()
            .watch_file
            .is_none());
        let args = Args::try_parse_from([
            "ddb-tui",
            "connect",
            "--api",
            "http://localhost",
            "--watch-file",
            "new-watches.json",
        ])
        .unwrap();
        assert_eq!(args.watch_file, Some(PathBuf::from("new-watches.json")));
    }
    #[test]
    fn value_presentation_is_explicit_and_global() {
        use super::*;
        assert_eq!(
            Args::try_parse_from(["ddb-tui"]).unwrap().values_formatting,
            ValuesFormatting::PrettyPrinters
        );
        for (name, expected) in [
            ("pretty-printers", ValuesFormatting::PrettyPrinters),
            ("raw", ValuesFormatting::Raw),
            ("disabled", ValuesFormatting::Disabled),
        ] {
            let args = Args::try_parse_from([
                "ddb-tui",
                "connect",
                "--api",
                "http://localhost:5000",
                "--values-formatting",
                name,
            ])
            .unwrap();
            assert_eq!(args.values_formatting, expected);
        }
        assert!(Args::try_parse_from(["ddb-tui", "--values-formatting", "unknown"]).is_err());
    }

    use std::fs;

    use super::*;

    #[test]
    fn debugger_settings_are_explicit_and_global_for_managed_and_connected_modes() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.yaml");
        let config = directory.path().join("ddb.yaml");
        fs::write(&settings, "{}\n").unwrap();
        fs::write(&config, "Framework: unspecified\n").unwrap();
        for tail in [
            vec![config.to_str().unwrap()],
            vec!["connect", "--api", "http://127.0.0.1:7000"],
        ] {
            let mut arguments = vec!["ddb-tui"];
            arguments.extend(tail);
            arguments.extend(["--debugger-settings", settings.to_str().unwrap()]);
            let args = Args::try_parse_from(arguments).unwrap();
            assert_eq!(
                args.debugger_settings,
                Some(settings.canonicalize().unwrap())
            );
            assert!(args.parse_mode().is_ok());
        }
    }

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

    #[test]
    fn local_workspace_is_independent_of_backend_startup_mode() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace.yaml");
        fs::write(&workspace, "roots: []\n").unwrap();
        let path = workspace.to_str().unwrap();
        for args in [
            vec![
                "ddb-tui",
                "--source-workspace",
                path,
                "connect",
                "--api",
                "http://127.0.0.1:5000",
            ],
            vec![
                "ddb-tui",
                "connect",
                "--api",
                "http://127.0.0.1:5000",
                "--source-workspace",
                path,
            ],
            vec![
                "ddb-tui",
                "--source-workspace",
                path,
                "launch",
                "--",
                "/bin/true",
            ],
        ] {
            let args = Args::try_parse_from(args).unwrap();
            assert_eq!(args.source_workspace.as_deref(), Some(workspace.as_path()));
            args.parse_mode().unwrap();
        }
    }
}
