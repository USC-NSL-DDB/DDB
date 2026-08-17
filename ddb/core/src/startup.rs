use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{
    arg::{AttachArgs, BackendArg, LaunchArgs, ServeArgs, ServeSession},
    common::config::{
        Config, DebuggerBackendKind, Framework, OnExit, StaticSessionConfig, StaticSessionStartMode,
    },
};

const STARTUP_REPORT_PROTOCOL_VERSION: u32 = 1;
const MAX_STARTUP_REPORT_BYTES: usize = 16 * 1024;
const MAX_STARTUP_MESSAGE_CHARS: usize = 4 * 1024;

#[derive(Debug)]
pub(crate) struct BackendStartup {
    pub config: Config,
    pub interactive: bool,
    pub allow_ephemeral_api_port: bool,
    pub preflight_debugger: bool,
    pub remove_auth_token_after_load: bool,
    pub reporter: Option<StartupReporter>,
}

impl BackendStartup {
    pub(crate) fn legacy(config: Config) -> Self {
        Self {
            config,
            interactive: true,
            allow_ephemeral_api_port: false,
            preflight_debugger: false,
            remove_auth_token_after_load: false,
            reporter: None,
        }
    }

    pub(crate) fn serve(args: &ServeArgs, reporter: Option<StartupReporter>) -> Result<Self> {
        crate::arg::validate_serve_args(args)?;
        let reporter = reporter.or_else(|| {
            args.startup_report
                .as_ref()
                .map(|path| StartupReporter::new(path.clone()))
        });
        if let Some(reporter) = &reporter {
            reporter.set_phase("config_loading");
        }

        let mut config = match (&args.config, &args.session) {
            (Some(path), None) => Config::from_file(path)
                .with_context(|| format!("failed to load configuration {}", path.display()))?,
            (None, Some(ServeSession::Launch(launch))) => launch_config(launch, args)?,
            (None, Some(ServeSession::Attach(attach))) => attach_config(attach, args),
            _ => unreachable!("validated serve arguments have one startup source"),
        };

        if args.managed {
            config.conf.api_server_bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
            config.conf.api_server_port = 0;
            config.conf.api_insecure_allow_remote = false;
            config.conf.api_tls_terminated_by_trusted_proxy = false;
        }
        if let Some(bind) = args.api_bind {
            config.conf.api_server_bind = bind;
        }
        if let Some(port) = args.api_port {
            config.conf.api_server_port = port;
        }
        if let Some(path) = &args.api_auth_token_file {
            config.conf.api_auth_token_file = Some(path.to_string_lossy().into_owned());
            config.conf.api_insecure_allow_unauthenticated_v2 = false;
        }

        if let Some(reporter) = &reporter {
            reporter.set_phase("config_validation");
        }
        Ok(Self {
            allow_ephemeral_api_port: config.conf.api_server_port == 0,
            config,
            interactive: false,
            preflight_debugger: true,
            remove_auth_token_after_load: args.managed,
            reporter,
        })
    }
}

fn launch_config(args: &LaunchArgs, serve: &ServeArgs) -> Result<Config> {
    let executable = args
        .command
        .first()
        .context("launch requires an executable after --")?;
    let executable = PathBuf::from(executable).canonicalize().with_context(|| {
        format!(
            "failed to resolve debuggee {}",
            PathBuf::from(executable).display()
        )
    })?;
    let metadata = fs::metadata(&executable)
        .with_context(|| format!("failed to inspect debuggee {}", executable.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("debuggee {} is not a regular file", executable.display());
    }

    let binary_path = executable
        .to_str()
        .context("debuggee path is not valid UTF-8")?
        .to_string();
    let binary_args = args
        .command
        .iter()
        .skip(1)
        .map(|arg| {
            arg.to_str()
                .map(str::to_string)
                .context("debuggee arguments must be valid UTF-8")
        })
        .collect::<Result<Vec<_>>>()?;
    let alias = executable
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("debuggee")
        .to_string();

    let mut config = generated_config(args.backend, OnExit::KILL, serve);
    config.static_sessions.push(StaticSessionConfig {
        tag: alias.clone(),
        alias,
        hash: "local-launch".to_string(),
        pid: u64::from(std::process::id()),
        ip: Ipv4Addr::LOCALHOST,
        start_mode: StaticSessionStartMode::Binary,
        binary_path,
        binary_args,
        stop_at_entry: args.should_stop_at_entry(),
        ..StaticSessionConfig::default()
    });
    Ok(config)
}

fn attach_config(args: &AttachArgs, serve: &ServeArgs) -> Config {
    let mut config = generated_config(args.backend, OnExit::DETACH, serve);
    let alias = format!("pid-{}", args.pid);
    config.static_sessions.push(StaticSessionConfig {
        tag: alias.clone(),
        alias,
        hash: "local-attach".to_string(),
        pid: args.pid,
        ip: Ipv4Addr::LOCALHOST,
        start_mode: StaticSessionStartMode::Attach,
        ..StaticSessionConfig::default()
    });
    config
}

fn generated_config(backend: BackendArg, on_exit: OnExit, serve: &ServeArgs) -> Config {
    let mut config = Config {
        framework: Framework::Unspecified,
        ..Config::default()
    };
    config.conf.auto_shutdown = false;
    config.conf.on_exit = on_exit;
    config.conf.debugger.backend = match backend {
        BackendArg::Gdb => DebuggerBackendKind::Gdb,
        BackendArg::Lldb => DebuggerBackendKind::Lldb,
    };

    if let Some(parent) = serve
        .startup_report
        .as_deref()
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        config.conf.base_dir = parent.join("state").to_string_lossy().into_owned();
        config.conf.log_dir = parent.join("logs").to_string_lossy().into_owned();
    }
    config
}

#[derive(Clone, Debug)]
pub(crate) struct StartupReporter {
    inner: Arc<ReporterInner>,
}

#[derive(Debug)]
struct ReporterInner {
    path: PathBuf,
    state: Mutex<ReporterState>,
}

#[derive(Debug)]
struct ReporterState {
    phase: &'static str,
    written: bool,
}

impl StartupReporter {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(ReporterInner {
                path,
                state: Mutex::new(ReporterState {
                    phase: "argument_validation",
                    written: false,
                }),
            }),
        }
    }

    pub(crate) fn set_phase(&self, phase: &'static str) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !state.written {
            state.phase = phase;
        }
    }

    pub(crate) fn ready(&self, endpoint: SocketAddr, server_instance_id: &str) -> Result<()> {
        self.write_once(StartupReport {
            protocol_version: STARTUP_REPORT_PROTOCOL_VERSION,
            status: "ready",
            phase: Some("service_ready"),
            code: None,
            message: None,
            pid: Some(std::process::id()),
            endpoint: Some(format!("http://{endpoint}")),
            server_instance_id: Some(server_instance_id),
            api_versions: Some(&["v2"]),
            backend_version: Some(env!("CARGO_PKG_VERSION")),
        })
    }

    pub(crate) fn failed(&self, error: &anyhow::Error) -> Result<()> {
        let phase = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|state| state.into_inner())
            .phase;
        let message = truncate_message(&format!("{error:#}"));
        self.write_once(StartupReport {
            protocol_version: STARTUP_REPORT_PROTOCOL_VERSION,
            status: "failed",
            phase: Some(phase),
            code: Some(startup_error_code(phase)),
            message: Some(&message),
            pid: Some(std::process::id()),
            endpoint: None,
            server_instance_id: None,
            api_versions: None,
            backend_version: Some(env!("CARGO_PKG_VERSION")),
        })
    }

    fn write_once(&self, report: StartupReport<'_>) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.written {
            return Ok(());
        }
        write_report(&self.inner.path, &report)?;
        state.written = true;
        Ok(())
    }
}

#[derive(Serialize)]
struct StartupReport<'a> {
    protocol_version: u32,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_instance_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_versions: Option<&'a [&'a str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend_version: Option<&'a str>,
}

fn write_report(path: &Path, report: &StartupReport<'_>) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        anyhow::bail!(
            "startup report parent directory {} does not exist",
            parent.display()
        );
    }
    if path.exists() {
        anyhow::bail!("startup report {} already exists", path.display());
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ddb-startup.json");
    let temporary = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let encoded = serde_json::to_vec(report).context("failed to encode startup report")?;
    if encoded.len() > MAX_STARTUP_REPORT_BYTES {
        anyhow::bail!(
            "startup report exceeds the {} byte limit",
            MAX_STARTUP_REPORT_BYTES
        );
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create startup report {}", temporary.display()))?;
        file.write_all(&encoded)
            .context("failed to write startup report")?;
        file.write_all(b"\n")
            .context("failed to terminate startup report")?;
        file.sync_all().context("failed to flush startup report")?;
        fs::hard_link(&temporary, path)
            .with_context(|| format!("failed to publish startup report {}", path.display()))?;
        fs::remove_file(&temporary).context("failed to remove startup report staging link")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn truncate_message(message: &str) -> String {
    let mut output = message
        .chars()
        .take(MAX_STARTUP_MESSAGE_CHARS)
        .collect::<String>();
    if message.chars().count() > MAX_STARTUP_MESSAGE_CHARS {
        output.push('…');
    }
    output
}

fn startup_error_code(phase: &str) -> &'static str {
    match phase {
        "config_loading" | "config_validation" => "CONFIG_INVALID",
        "debugger_resolution" => "DEBUGGER_UNAVAILABLE",
        "filesystem_setup" => "SETUP_FAILED",
        "auth_setup" => "AUTH_SETUP_FAILED",
        "api_bind" => "API_BIND_FAILED",
        "service_startup" => "SERVICE_START_FAILED",
        _ => "STARTUP_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;

    use super::*;
    use crate::arg::{Args, Command};

    #[test]
    fn managed_overrides_force_ephemeral_authenticated_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("ddb.yaml");
        let token_path = dir.path().join("tokens.json");
        let report_path = dir.path().join("startup.json");
        fs::write(
            &config_path,
            "Framework: unspecified\nConf:\n  api_server_bind: 0.0.0.0\n  api_server_port: 5000\n  api_insecure_allow_unauthenticated_v2: true\n",
        )
        .unwrap();
        fs::write(&token_path, "{\"tokens\":[]}").unwrap();

        let parsed = Args::try_parse_from([
            "ddb",
            "serve",
            config_path.to_str().unwrap(),
            "--managed",
            "--api-auth-token-file",
            token_path.to_str().unwrap(),
            "--startup-report",
            report_path.to_str().unwrap(),
        ])
        .unwrap();
        let Some(Command::Serve(args)) = parsed.command else {
            panic!("serve should parse");
        };
        let startup = BackendStartup::serve(&args, None).unwrap();
        assert!(startup.config.conf.api_server_bind.is_loopback());
        assert_eq!(startup.config.conf.api_server_port, 0);
        assert!(!startup.config.conf.api_insecure_allow_unauthenticated_v2);
        assert!(startup.allow_ephemeral_api_port);
    }

    #[test]
    fn launch_shortcut_preserves_exact_arguments_and_kill_policy() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("program");
        fs::write(&executable, b"test").unwrap();
        let token = dir.path().join("tokens.json");
        fs::write(&token, "{\"tokens\":[]}").unwrap();
        let report = dir.path().join("startup.json");

        let parsed = Args::try_parse_from([
            "ddb",
            "serve",
            "--api-auth-token-file",
            token.to_str().unwrap(),
            "--startup-report",
            report.to_str().unwrap(),
            "launch",
            "--",
            executable.to_str().unwrap(),
            "--leading",
            "argument with spaces",
        ])
        .unwrap();
        let Some(Command::Serve(args)) = parsed.command else {
            panic!("serve should parse");
        };
        let startup = BackendStartup::serve(&args, None).unwrap();
        let session = &startup.config.static_sessions[0];
        assert_eq!(session.binary_args, ["--leading", "argument with spaces"]);
        assert!(session.stop_at_entry);
        assert_eq!(startup.config.conf.on_exit, OnExit::KILL);
    }

    #[test]
    fn startup_report_is_atomic_and_single_assignment() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("startup.json");
        let reporter = StartupReporter::new(report.clone());
        reporter
            .ready("127.0.0.1:43210".parse().unwrap(), "server")
            .unwrap();
        reporter
            .failed(&anyhow::anyhow!("must not replace ready"))
            .unwrap();

        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        assert_eq!(document["status"], "ready");
        assert_eq!(document["endpoint"], "http://127.0.0.1:43210");
        assert_eq!(document["server_instance_id"], "server");
        assert_eq!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn startup_report_never_replaces_a_broken_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("startup.json");
        symlink("missing-target", &report).unwrap();
        let reporter = StartupReporter::new(report.clone());
        let error = reporter
            .ready("127.0.0.1:43210".parse().unwrap(), "server")
            .unwrap_err();

        assert!(format!("{error:#}").contains("failed to publish startup report"));
        assert_eq!(
            fs::read_link(&report).unwrap(),
            PathBuf::from("missing-target")
        );
        assert_eq!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0
        );
    }
}
