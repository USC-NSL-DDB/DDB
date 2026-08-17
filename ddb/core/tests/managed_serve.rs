use std::{
    collections::HashSet,
    fs,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use ddb_api_client::{
    v2::{self, target},
    ClientConfig, DdbClient,
};
use serde_json::Value;
use tempfile::TempDir;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_TOKEN: &str = "control-token-for-managed-serve-tests-000000000000";
const ADMIN_TOKEN: &str = "admin-token-for-managed-serve-tests-00000000000000";

struct ManagedProcess {
    _directory: TempDir,
    child: Child,
    report: Value,
    stopped: bool,
}

impl ManagedProcess {
    fn start_mock() -> Self {
        let directory = tempfile::tempdir().expect("temporary managed directory should exist");
        let token_path = directory.path().join("tokens.json");
        let config_path = directory.path().join("ddb.yaml");
        let report_path = directory.path().join("startup.json");
        write_tokens(&token_path);
        fs::write(
            &config_path,
            format!(
                r#"Framework: unspecified
Conf:
  auto_shutdown: false
  base_dir: "{}"
  log_dir: "{}"
  Debugger:
    backend: mock
StaticSessions:
  - tag: managed
    alias: managed
    hash: managed-group
    pid: 4201
"#,
                directory.path().join("state").display(),
                directory.path().join("logs").display()
            ),
        )
        .expect("managed config should be written");

        let mut child = Command::new(env!("CARGO_BIN_EXE_ddb"))
            .arg("serve")
            .arg(&config_path)
            .arg("--managed")
            .arg("--api-auth-token-file")
            .arg(&token_path)
            .arg("--startup-report")
            .arg(&report_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("managed DDB should spawn");
        let report = wait_for_report(&report_path, &mut child);
        assert_eq!(report["status"], "ready");
        assert_eq!(report["pid"].as_u64(), Some(u64::from(child.id())));
        assert!(
            !token_path.exists(),
            "managed DDB must unlink credentials after loading them"
        );
        Self {
            _directory: directory,
            child,
            report,
            stopped: false,
        }
    }

    fn endpoint(&self) -> &str {
        self.report["endpoint"]
            .as_str()
            .expect("ready report should contain endpoint")
    }

    async fn shutdown(&mut self) -> ExitStatus {
        let client = DdbClient::new(
            ClientConfig::new(self.endpoint()).with_bearer_token(ADMIN_TOKEN.to_string()),
        )
        .expect("admin client should build");
        client
            .shutdown(v2::ShutdownRequest {
                context: Some(v2::RequestContext {
                    idempotency_key: Some(format!(
                        "managed_serve_test_{}",
                        uuid::Uuid::new_v4().simple()
                    )),
                    ..Default::default()
                }),
                target: Some(v2::Target {
                    selector: Some(target::Selector::Broadcast(v2::BroadcastTarget {})),
                }),
                ..Default::default()
            })
            .await
            .expect("admin shutdown should succeed");
        let status = wait_for_exit(&mut self.child);
        self.stopped = true;
        status
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        self.stopped = true;
    }
}

#[tokio::test]
async fn managed_serve_is_headless_authenticated_and_reports_actual_endpoint() {
    let mut process = ManagedProcess::start_mock();
    assert!(
        process.child.try_wait().unwrap().is_none(),
        "closed stdin must not stop headless serve mode"
    );

    let address = process
        .endpoint()
        .strip_prefix("http://")
        .expect("managed endpoint should use loopback HTTP")
        .parse::<SocketAddr>()
        .expect("managed endpoint should be a socket address");
    assert!(address.ip().is_loopback());
    assert_ne!(address.port(), 0);
    assert_eq!(process.report["api_versions"][0], "v2");

    let unauthenticated = DdbClient::new(ClientConfig::new(process.endpoint()))
        .expect("unauthenticated client should build");
    assert!(
        unauthenticated.handshake().await.is_err(),
        "managed API must reject requests without its bearer token"
    );

    let client = DdbClient::new(
        ClientConfig::new(process.endpoint()).with_bearer_token(CONTROL_TOKEN.to_string()),
    )
    .expect("control client should build");
    let (server, capabilities) = client
        .handshake()
        .await
        .expect("authenticated v2 handshake should succeed");
    assert_eq!(
        process.report["server_instance_id"].as_str(),
        Some(server.server_instance_id.as_str())
    );
    assert_eq!(capabilities.server_instance_id, server.server_instance_id);
    assert!(process.shutdown().await.success());
}

#[tokio::test]
async fn concurrent_managed_servers_never_collide_on_ports() {
    let workers = (0..50)
        .map(|_| std::thread::spawn(ManagedProcess::start_mock))
        .collect::<Vec<_>>();
    let mut processes = workers
        .into_iter()
        .map(|worker| {
            worker
                .join()
                .expect("managed server worker should not panic")
        })
        .collect::<Vec<_>>();
    let endpoints = processes
        .iter()
        .map(|process| process.endpoint().to_string())
        .collect::<HashSet<_>>();
    assert_eq!(endpoints.len(), 50);

    for process in &mut processes {
        assert!(process.shutdown().await.success());
    }
}

#[test]
fn malformed_config_publishes_a_structured_failure_report() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("tokens.json");
    let config_path = directory.path().join("invalid.yaml");
    let report_path = directory.path().join("startup.json");
    write_tokens(&token_path);
    fs::write(&config_path, "Conf: [not-valid\n").unwrap();

    let mut child = spawn_managed(&config_path, &token_path, &report_path);
    let report = wait_for_report(&report_path, &mut child);
    let status = wait_for_exit(&mut child);
    assert!(!status.success());
    assert_eq!(report["status"], "failed");
    assert_eq!(report["phase"], "config_loading");
    assert_eq!(report["code"], "CONFIG_INVALID");
    assert!(report["message"]
        .as_str()
        .is_some_and(|message| message.contains("failed to load configuration")));
}

#[test]
fn startup_report_is_create_new_and_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("tokens.json");
    let config_path = directory.path().join("ddb.yaml");
    let report_path = directory.path().join("startup.json");
    write_tokens(&token_path);
    fs::write(
        &config_path,
        format!(
            "Framework: unspecified\nConf:\n  auto_shutdown: false\n  base_dir: {:?}\n  log_dir: {:?}\n  Debugger:\n    backend: mock\n",
            directory.path().join("state").to_string_lossy(),
            directory.path().join("logs").to_string_lossy()
        ),
    )
    .unwrap();
    fs::write(&report_path, b"sentinel\n").unwrap();

    let mut child = spawn_managed(&config_path, &token_path, &report_path);
    let status = wait_for_exit(&mut child);
    assert!(!status.success());
    assert_eq!(fs::read(&report_path).unwrap(), b"sentinel\n");
}

#[test]
fn unavailable_debugger_publishes_a_structured_failure_report() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("tokens.json");
    let config_path = directory.path().join("ddb.yaml");
    let report_path = directory.path().join("startup.json");
    write_tokens(&token_path);
    fs::write(
        &config_path,
        format!(
            "Framework: unspecified\nConf:\n  auto_shutdown: false\n  base_dir: {:?}\n  log_dir: {:?}\n  Debugger:\n    backend: gdb\n",
            directory.path().join("state").to_string_lossy(),
            directory.path().join("logs").to_string_lossy()
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ddb"))
        .arg("serve")
        .arg(&config_path)
        .arg("--managed")
        .arg("--api-auth-token-file")
        .arg(&token_path)
        .arg("--startup-report")
        .arg(&report_path)
        .env("PATH", "")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("managed DDB should spawn");
    let report = wait_for_report(&report_path, &mut child);
    let status = wait_for_exit(&mut child);
    assert!(!status.success());
    assert_eq!(report["status"], "failed");
    assert_eq!(report["phase"], "debugger_resolution");
    assert_eq!(report["code"], "DEBUGGER_UNAVAILABLE");
    assert!(report["message"]
        .as_str()
        .is_some_and(|message| message.contains("gdb") && message.contains("unavailable")));
}

#[test]
#[cfg(unix)]
fn invalid_static_session_publishes_a_structured_failure_report() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("tokens.json");
    let config_path = directory.path().join("ddb.yaml");
    let report_path = directory.path().join("startup.json");
    let fake_gdb = directory.path().join("gdb");
    write_tokens(&token_path);
    fs::write(&fake_gdb, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&fake_gdb).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    fs::set_permissions(&fake_gdb, permissions).unwrap();
    fs::write(
        &config_path,
        format!(
            "Framework: unspecified\nConf:\n  auto_shutdown: false\n  base_dir: {:?}\n  log_dir: {:?}\n  Debugger:\n    backend: gdb\nStaticSessions:\n  - tag: invalid-attach\n    alias: invalid-attach\n    hash: invalid-group\n    pid: 0\n",
            directory.path().join("state").to_string_lossy(),
            directory.path().join("logs").to_string_lossy()
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ddb"))
        .arg("serve")
        .arg(&config_path)
        .arg("--managed")
        .arg("--api-auth-token-file")
        .arg(&token_path)
        .arg("--startup-report")
        .arg(&report_path)
        .env("PATH", directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("managed DDB should spawn");
    let report = wait_for_report(&report_path, &mut child);
    let status = wait_for_exit(&mut child);
    assert!(!status.success());
    assert_eq!(report["status"], "failed");
    assert_eq!(report["phase"], "config_validation");
    assert_eq!(report["code"], "CONFIG_INVALID");
    assert!(report["message"].as_str().is_some_and(|message| message
        .contains("StaticSessions[0]")
        && message.contains("non-zero")));
}

#[test]
fn occupied_api_port_publishes_a_structured_bind_failure() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("tokens.json");
    let config_path = directory.path().join("ddb.yaml");
    let report_path = directory.path().join("startup.json");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied_port = listener.local_addr().unwrap().port();
    write_tokens(&token_path);
    fs::write(
        &config_path,
        format!(
            "Framework: unspecified\nConf:\n  auto_shutdown: false\n  base_dir: {:?}\n  log_dir: {:?}\n  Debugger:\n    backend: mock\n",
            directory.path().join("state").to_string_lossy(),
            directory.path().join("logs").to_string_lossy()
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ddb"))
        .arg("serve")
        .arg(&config_path)
        .arg("--api-bind")
        .arg("127.0.0.1")
        .arg("--api-port")
        .arg(occupied_port.to_string())
        .arg("--api-auth-token-file")
        .arg(&token_path)
        .arg("--startup-report")
        .arg(&report_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("DDB serve should spawn");
    let report = wait_for_report(&report_path, &mut child);
    let status = wait_for_exit(&mut child);
    assert!(!status.success());
    assert_eq!(report["status"], "failed");
    assert_eq!(report["phase"], "api_bind");
    assert_eq!(report["code"], "API_BIND_FAILED");
    assert!(report["message"]
        .as_str()
        .is_some_and(|message| message.contains("bind")));
}

fn spawn_managed(config: &Path, tokens: &Path, report: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_ddb"))
        .arg("serve")
        .arg(config)
        .arg("--managed")
        .arg("--api-auth-token-file")
        .arg(tokens)
        .arg("--startup-report")
        .arg(report)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("managed DDB should spawn")
}

fn write_tokens(path: &Path) {
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "tokens": [
                {"token": CONTROL_TOKEN, "scope": "control"},
                {"token": ADMIN_TOKEN, "scope": "admin"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
}

fn wait_for_report(path: &PathBuf, child: &mut Child) -> Value {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if path.exists() {
            return serde_json::from_slice(&fs::read(path).unwrap())
                .expect("published startup report should be valid JSON");
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "DDB exited with {status} before publishing {}",
                path.display()
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for startup report {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out waiting for DDB process to exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
