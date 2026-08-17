mod report;
mod resolve;
mod runtime_dir;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ddb_api_client::{
    v2::{self, target},
    ClientConfig, DdbClient,
};
use tokio::process::{Child, Command};

use crate::cli::{Args, DebuggerBackend, Mode};

use self::runtime_dir::RuntimeFiles;

const REPORT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const API_SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const TERM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
enum SessionSpec {
    Config(PathBuf),
    Launch {
        backend: DebuggerBackend,
        stop_at_entry: bool,
        command: Vec<OsString>,
    },
    Attach {
        backend: DebuggerBackend,
        pid: u64,
    },
}

pub(crate) struct ManagedBackend {
    child: Option<Child>,
    files: RuntimeFiles,
    endpoint: String,
    server_instance_id: String,
    backend_version: String,
    api_versions: Vec<String>,
}

impl ManagedBackend {
    pub(crate) async fn start(args: &Args, mode: &Mode) -> Result<Self> {
        let session = match mode {
            Mode::ManagedConfig(path) => SessionSpec::Config(path.clone()),
            Mode::ManagedLaunch {
                backend,
                stop_at_entry,
                command,
            } => SessionSpec::Launch {
                backend: *backend,
                stop_at_entry: *stop_at_entry,
                command: command.clone(),
            },
            Mode::ManagedAttach { backend, pid } => SessionSpec::Attach {
                backend: *backend,
                pid: *pid,
            },
            Mode::Connect { .. } => {
                anyhow::bail!("external connect mode does not own a DDB backend")
            }
        };

        let executable = resolve::resolve(args.ddb_path.as_deref())?;
        let mut files = RuntimeFiles::new(args.backend_log.as_deref())?;
        let (stdout, stderr) = files.child_stdio()?;
        let mut command = build_command(&executable, &files, &session);
        command
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(target_os = "linux")]
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Close the race where the parent exits between fork and
                // PR_SET_PDEATHSIG: an already-reparented child must not run.
                if libc::getppid() == 1 {
                    return Err(std::io::Error::other(
                        "ddb-tui exited while managed DDB was starting",
                    ));
                }
                Ok(())
            });
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start managed DDB {}", executable.display()))?;
        let pid = child
            .id()
            .context("managed DDB did not expose a process identifier")?;

        let report =
            match wait_for_report(&mut child, files.report_path(), pid, args.startup_timeout())
                .await
            {
                Ok(report) => report,
                Err(error) => {
                    terminate_child(&mut child, pid).await;
                    let tail = files.tail_log();
                    files.preserve_log();
                    let log_path = files.log_path().display().to_string();
                    let mut message = format!("{error:#}\nmanaged DDB log retained at {log_path}");
                    if !tail.trim().is_empty() {
                        message.push_str("\nbackend log tail:\n");
                        message.push_str(tail.trim_end());
                    }
                    return Err(anyhow::anyhow!(message));
                }
            };

        Ok(Self {
            child: Some(child),
            endpoint: report.endpoint,
            server_instance_id: report.server_instance_id,
            backend_version: report.backend_version,
            api_versions: report.api_versions,
            files,
        })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn control_token(&self) -> &str {
        self.files.control_token()
    }

    pub(crate) fn server_instance_id(&self) -> &str {
        &self.server_instance_id
    }

    pub(crate) fn backend_version(&self) -> &str {
        &self.backend_version
    }

    pub(crate) fn api_versions(&self) -> &[String] {
        &self.api_versions
    }

    pub(crate) fn log_path(&self) -> &Path {
        self.files.log_path()
    }

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        if let Some(status) = child.try_wait()? {
            self.child.take();
            if status.success() {
                return Ok(());
            }
            self.files.preserve_log();
            anyhow::bail!(
                "managed DDB exited with {status}; backend log retained at {}",
                self.files.log_path().display()
            );
        }

        let client = DdbClient::new(
            ClientConfig::new(&self.endpoint)
                .with_bearer_token(self.files.admin_token().to_string()),
        )?;
        let api_result = tokio::time::timeout(
            API_SHUTDOWN_REQUEST_TIMEOUT,
            client.shutdown(v2::ShutdownRequest {
                context: Some(v2::RequestContext {
                    idempotency_key: Some(format!(
                        "ddb_tui_shutdown_{}",
                        uuid::Uuid::new_v4().simple()
                    )),
                    ..Default::default()
                }),
                target: Some(v2::Target {
                    selector: Some(target::Selector::Broadcast(v2::BroadcastTarget {})),
                }),
                ..Default::default()
            }),
        )
        .await;
        let api_detail = match api_result {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(format!("API shutdown failed: {error}")),
            Err(_) => Some(format!(
                "API shutdown timed out after {:?}",
                API_SHUTDOWN_REQUEST_TIMEOUT
            )),
        };

        match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, child.wait()).await {
            Ok(wait_result) => {
                let status = wait_result.context("failed to reap managed DDB")?;
                self.child.take();
                if status.success() {
                    return Ok(());
                }
                self.files.preserve_log();
                anyhow::bail!(
                    "managed DDB exited with {status}; backend log retained at {}",
                    self.files.log_path().display()
                );
            }
            Err(_) => {
                let api_detail = api_detail
                    .map(|detail| format!("; {detail}"))
                    .unwrap_or_default();
                terminate_child(child, child.id().unwrap_or_default()).await;
                self.child.take();
                self.files.preserve_log();
                anyhow::bail!(
                    "managed DDB did not stop within {:?}{api_detail}; backend log retained at {}",
                    GRACEFUL_SHUTDOWN_TIMEOUT,
                    self.files.log_path().display()
                );
            }
        }
    }
}

impl Drop for ManagedBackend {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            if let Some(pid) = child.id() {
                force_kill_process_group(pid);
            }
            let _ = child.start_kill();
        }
    }
}

fn build_command(executable: &Path, files: &RuntimeFiles, session: &SessionSpec) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("serve")
        .arg("--managed")
        .arg("--api-auth-token-file")
        .arg(files.token_path())
        .arg("--startup-report")
        .arg(files.report_path())
        .arg("--console-log");

    match session {
        SessionSpec::Config(path) => {
            command.arg(path);
        }
        SessionSpec::Launch {
            backend,
            stop_at_entry,
            command: debuggee,
        } => {
            command.arg("launch").arg("--backend").arg(backend.as_arg());
            if !stop_at_entry {
                command.arg("--no-stop-at-entry");
            }
            command.arg("--").args(debuggee);
        }
        SessionSpec::Attach { backend, pid } => {
            command
                .arg("attach")
                .arg("--backend")
                .arg(backend.as_arg())
                .arg("--pid")
                .arg(pid.to_string());
        }
    }
    command
}

async fn wait_for_report(
    child: &mut Child,
    report_path: &Path,
    pid: u32,
    timeout: Duration,
) -> Result<report::ReadyReport> {
    let deadline = Instant::now() + timeout;
    loop {
        if report_path.exists() {
            return report::read(report_path, pid);
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect managed DDB status")?
        {
            anyhow::bail!("managed DDB exited before readiness with {status}");
        }
        if Instant::now() >= deadline {
            anyhow::bail!("managed DDB startup timed out after {timeout:?}");
        }
        tokio::time::sleep(REPORT_POLL_INTERVAL).await;
    }
}

async fn terminate_child(child: &mut Child, pid: u32) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    terminate_process_group(pid);
    if matches!(
        tokio::time::timeout(TERM_SHUTDOWN_TIMEOUT, child.wait()).await,
        Ok(Ok(_))
    ) {
        return;
    }
    if pid != 0 {
        force_kill_process_group(pid);
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(unix)]
fn terminate_process_group(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: u32) {}

#[cfg(unix)]
fn force_kill_process_group(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn force_kill_process_group(_pid: u32) {}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::fs;

    use super::*;

    #[test]
    fn managed_command_never_contains_token_values() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("ddb.yaml");
        fs::write(&config, "Framework: unspecified\n").unwrap();
        let files = RuntimeFiles::new(None).unwrap();
        let command = build_command(Path::new("/tmp/ddb"), &files, &SessionSpec::Config(config));
        let rendered = format!("{command:?}");
        assert!(!rendered.contains(files.control_token()));
        assert!(!rendered.contains(files.admin_token()));
        assert!(rendered.contains("api-auth-token-file"));
    }

    #[test]
    fn launch_command_preserves_argument_boundaries() {
        let files = RuntimeFiles::new(None).unwrap();
        let command = build_command(
            Path::new("/tmp/ddb"),
            &files,
            &SessionSpec::Launch {
                backend: DebuggerBackend::Gdb,
                stop_at_entry: true,
                command: ["./app", "--flag", "value with spaces"]
                    .map(OsString::from)
                    .to_vec(),
            },
        );
        let arguments = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_os_string())
            .collect::<Vec<_>>();
        assert!(arguments
            .windows(3)
            .any(|window| window == ["./app", "--flag", "value with spaces"].map(OsString::from)));
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn early_backend_exit_is_reported_before_readiness() {
        let directory = tempfile::tempdir().unwrap();
        let fake_ddb = directory.path().join("ddb");
        let config = directory.path().join("ddb.yaml");
        let backend_log = directory.path().join("backend.log");
        write_executable(&fake_ddb, "#!/bin/sh\nexit 23\n");
        fs::write(&config, "Framework: unspecified\n").unwrap();
        let args = Args::try_parse_from([
            "ddb-tui",
            "--ddb-path",
            fake_ddb.to_str().unwrap(),
            "--backend-log",
            backend_log.to_str().unwrap(),
            config.to_str().unwrap(),
        ])
        .unwrap();
        let mode = args.parse_mode().unwrap();

        let error = ManagedBackend::start(&args, &mode)
            .await
            .err()
            .expect("early exit should fail startup");
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("exited before readiness"));
        assert!(diagnostic.contains("23"));
        assert!(diagnostic.contains(backend_log.to_str().unwrap()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_timeout_terminates_and_reaps_backend_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let fake_ddb = directory.path().join("ddb");
        let config = directory.path().join("ddb.yaml");
        let pid_file = directory.path().join("backend.pid");
        let backend_log = directory.path().join("backend.log");
        write_executable(
            &fake_ddb,
            &format!(
                "#!/bin/sh\necho $$ > {}\nexec sleep 30\n",
                pid_file.display()
            ),
        );
        fs::write(&config, "Framework: unspecified\n").unwrap();
        let args = Args::try_parse_from([
            "ddb-tui",
            "--ddb-path",
            fake_ddb.to_str().unwrap(),
            "--backend-log",
            backend_log.to_str().unwrap(),
            "--startup-timeout",
            "1",
            config.to_str().unwrap(),
        ])
        .unwrap();
        let mode = args.parse_mode().unwrap();

        let error = ManagedBackend::start(&args, &mode)
            .await
            .err()
            .expect("startup timeout should fail");
        assert!(format!("{error:#}").contains("startup timed out"));
        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "timed-out backend survived"
        );
    }
}
