#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use reqwest::blocking::{Client, Response};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tempfile::TempDir;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
pub const V2_TEST_READ_TOKEN: &str = "ddb-integration-read-token-0000000001";
pub const V2_TEST_CONTROL_TOKEN: &str = "ddb-integration-control-token-000001";
pub const V2_TEST_ADMIN_TOKEN: &str = "ddb-integration-admin-token-00000001";

#[derive(Default)]
struct OutputBuffer {
    lines: Mutex<Vec<String>>,
}

impl OutputBuffer {
    fn push(&self, line: String) {
        self.lines.lock().unwrap().push(line);
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

#[derive(Clone)]
pub struct SessionSpec<'a> {
    pub tag: &'a str,
    pub alias: &'a str,
    pub hash: &'a str,
    pub pid: u64,
    pub start_delay_ms: u64,
    pub source_file: &'a str,
    pub source_line: u64,
    pub function: &'a str,
    pub exit_on_continue: bool,
}

pub struct BinarySessionSpec<'a> {
    pub tag: &'a str,
    pub alias: &'a str,
    pub hash: &'a str,
    pub pid: u64,
    pub ip: &'a str,
    pub start_delay_ms: u64,
    pub binary_path: &'a str,
    pub binary_args: Vec<String>,
    pub stop_at_entry: bool,
}

pub struct AttachSessionSpec<'a> {
    pub tag: &'a str,
    pub alias: &'a str,
    pub hash: &'a str,
    pub pid: u64,
    pub ip: &'a str,
}

#[derive(Clone, Debug)]
pub struct BuiltRealExample {
    pub manifest_path: PathBuf,
    pub binary_path: PathBuf,
    pub source_path: PathBuf,
    pub breakpoint_line: u64,
}

#[derive(Clone, Debug)]
pub struct BuiltRealBinaryExample {
    pub manifest_path: PathBuf,
    pub binary_path: PathBuf,
    pub source_path: PathBuf,
}

pub struct DdbProcess {
    _tempdir: TempDir,
    child: Child,
    stdin: ChildStdin,
    stdout: Arc<OutputBuffer>,
    stderr: Arc<OutputBuffer>,
    client: Client,
    port: u16,
    grpc_port: Option<u16>,
    stopped: bool,
}

impl DdbProcess {
    pub fn spawn(sessions: &[SessionSpec<'_>]) -> Self {
        let config_contents = render_mock_config(sessions, false);
        Self::spawn_with_config("ddb-integration.yaml", config_contents)
    }

    pub fn spawn_with_v2_auth(sessions: &[SessionSpec<'_>]) -> Self {
        let config_contents = with_v2_auth(render_mock_config(sessions, false));
        Self::spawn_with_config("ddb-v2-integration.yaml", config_contents)
    }

    /// Starts an authenticated v2 server with additional `Conf` entries.
    ///
    /// This is intentionally limited to integration tests so deployment-policy
    /// behavior is exercised at the process boundary rather than inferred from
    /// configuration-unit tests.
    pub fn spawn_with_v2_conf(sessions: &[SessionSpec<'_>], conf_entries: &str) -> Self {
        let config_contents = with_conf_entries(
            with_v2_auth(render_mock_config(sessions, false)),
            conf_entries,
        );
        Self::spawn_with_config("ddb-v2-policy-integration.yaml", config_contents)
    }

    #[cfg(feature = "grpc-preview")]
    pub fn spawn_with_v2_auth_and_grpc(sessions: &[SessionSpec<'_>]) -> Self {
        let config_contents = with_grpc_preview(with_v2_auth(render_mock_config(sessions, false)));
        Self::spawn_with_config("ddb-v2-grpc-integration.yaml", config_contents)
    }

    pub fn spawn_with_v2_auth_rejection(
        sessions: &[SessionSpec<'_>],
        rejected_tag: &str,
        rejected_command: &str,
    ) -> Self {
        let config_contents = with_v2_auth(render_mock_config_with_rejection(
            sessions,
            false,
            rejected_tag,
            rejected_command,
        ));
        Self::spawn_with_config("ddb-v2-rejection-integration.yaml", config_contents)
    }

    pub fn spawn_with_bootstrap_exit(sessions: &[SessionSpec<'_>]) -> Self {
        let config_contents = render_mock_config(sessions, true);
        Self::spawn_with_config("ddb-bootstrap-exit-integration.yaml", config_contents)
    }

    pub fn spawn_real_binary_sessions(sessions: &[BinarySessionSpec<'_>]) -> Self {
        let config_contents = render_real_binary_config(sessions, "gdb");
        Self::spawn_with_config("ddb-real-integration.yaml", config_contents)
    }

    pub fn spawn_real_binary_sessions_with_v2_auth(
        backend: &str,
        sessions: &[BinarySessionSpec<'_>],
    ) -> Self {
        ensure_debugger_environment(backend);
        let config_contents = with_v2_auth(render_real_binary_config(sessions, backend));
        let environment = if backend == "lldb" {
            vec![("FAKETIME", "-00000000000000000000.000000000")]
        } else {
            Vec::new()
        };
        Self::spawn_with_config_and_env(
            "ddb-real-v2-integration.yaml",
            config_contents,
            &environment,
        )
    }
    pub fn spawn_faketime_binary_sessions(
        backend: &str,
        sessions: &[BinarySessionSpec<'_>],
        libfaketime: &Path,
    ) -> Self {
        ensure_debugger_environment(backend);
        let config_contents = render_real_binary_config(sessions, backend);
        let libfaketime = libfaketime
            .to_str()
            .expect("libfaketime path should be valid utf-8");
        Self::spawn_with_config_and_env(
            "ddb-real-faketime-integration.yaml",
            config_contents,
            &[
                ("LD_PRELOAD", libfaketime),
                ("FAKETIME", "-00000000000000000000.000000000"),
                ("FAKETIME_DONT_FAKE_MONOTONIC", "1"),
                ("FAKETIME_DISABLE_SHM", "1"),
            ],
        )
    }

    pub fn spawn_lldb_binary_sessions(sessions: &[BinarySessionSpec<'_>]) -> Self {
        ensure_debugger_environment("lldb");
        let config_contents = render_real_binary_config(sessions, "lldb");
        Self::spawn_with_config_and_env(
            "ddb-real-lldb-integration.yaml",
            config_contents,
            &[("FAKETIME", "-00000000000000000000.000000000")],
        )
    }

    pub fn spawn_attach_sessions(backend: &str, sessions: &[AttachSessionSpec<'_>]) -> Self {
        ensure_debugger_environment(backend);
        let config_contents = render_real_attach_config(sessions, backend);
        Self::spawn_with_config("ddb-real-attach-integration.yaml", config_contents)
    }

    pub fn spawn_lldb_attach_sessions(sessions: &[AttachSessionSpec<'_>]) -> Self {
        Self::spawn_attach_sessions("lldb", sessions)
    }

    pub fn spawn_real_dbt_sessions(sessions: &[BinarySessionSpec<'_>]) -> Self {
        let config_contents = render_real_dbt_config(sessions, "gdb");
        Self::spawn_with_config("ddb-real-dbt-integration.yaml", config_contents)
    }

    pub fn spawn_lldb_dbt_sessions(sessions: &[BinarySessionSpec<'_>]) -> Self {
        ensure_debugger_environment("lldb");
        let config_contents = render_real_dbt_config(sessions, "lldb");
        Self::spawn_with_config("ddb-real-lldb-dbt-integration.yaml", config_contents)
    }

    fn spawn_with_config(config_name: &str, config_contents: String) -> Self {
        Self::spawn_with_config_and_env(config_name, config_contents, &[])
    }

    fn spawn_with_config_and_env(
        config_name: &str,
        config_contents: String,
        environment: &[(&str, &str)],
    ) -> Self {
        let port = reserve_port();
        let grpc_port = config_contents.contains("__GRPC_PORT__").then(reserve_port);
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = tempdir.path().join(config_name);
        let state_dir = tempdir.path().join("state");
        let log_dir = tempdir.path().join("logs");
        let token_path = tempdir.path().join("api-tokens.json");
        std::fs::write(
            &token_path,
            serde_json::to_vec(&json!({
                "tokens": [
                    {"token": V2_TEST_READ_TOKEN, "scope": "read"},
                    {"token": V2_TEST_CONTROL_TOKEN, "scope": "control"},
                    {"token": V2_TEST_ADMIN_TOKEN, "scope": "admin"}
                ]
            }))
            .expect("token document should serialize"),
        )
        .expect("token file should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))
                .expect("token file permissions should be restricted");
        }
        let config_contents = config_contents
            .replace("__API_PORT__", &port.to_string())
            .replace(
                "__API_TOKEN_FILE__",
                token_path
                    .to_str()
                    .expect("token path should be valid utf-8"),
            )
            .replace(
                "__BASE_DIR__",
                state_dir.to_str().expect("state dir should be valid utf-8"),
            )
            .replace(
                "__LOG_DIR__",
                log_dir.to_str().expect("log dir should be valid utf-8"),
            );
        let config_contents = match grpc_port {
            Some(grpc_port) => config_contents.replace("__GRPC_PORT__", &grpc_port.to_string()),
            None => config_contents,
        };
        std::fs::write(&config_path, config_contents).expect("config file should be written");

        let stdout = Arc::new(OutputBuffer::default());
        let stderr = Arc::new(OutputBuffer::default());
        let binary = std::env::var("CARGO_BIN_EXE_ddb")
            .ok()
            .or_else(|| option_env!("CARGO_BIN_EXE_ddb").map(str::to_string))
            .expect("ddb binary path should be set");

        let mut command = Command::new(binary);
        command
            .arg(config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("ddb should spawn");

        let stdin = child.stdin.take().expect("stdin should be piped");
        let child_stdout = child.stdout.take().expect("stdout should be piped");
        let child_stderr = child.stderr.take().expect("stderr should be piped");

        spawn_reader(child_stdout, stdout.clone());
        spawn_reader(child_stderr, stderr.clone());

        let client = Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .expect("reqwest client should build");

        let mut process = Self {
            _tempdir: tempdir,
            child,
            stdin,
            stdout,
            stderr,
            client,
            port,
            grpc_port,
            stopped: false,
        };
        process.wait_for_status_up();
        process
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn api_endpoint(&self) -> String {
        self.base_url()
    }

    #[cfg(feature = "grpc-preview")]
    pub fn grpc_endpoint(&self) -> String {
        format!(
            "http://127.0.0.1:{}",
            self.grpc_port
                .expect("process should have a configured gRPC preview port")
        )
    }

    pub fn send_cmd(&mut self, cmd: &str) {
        self.stdin
            .write_all(format!("{}\n", cmd).as_bytes())
            .expect("command should be written to stdin");
        self.stdin.flush().expect("stdin should flush");
    }

    pub fn api_get(&self, path: &str) -> Value {
        self.client
            .get(format!("{}{}", self.base_url(), path))
            .send()
            .expect("request should succeed")
            .error_for_status()
            .expect("status should be successful")
            .json()
            .expect("response body should be json")
    }

    pub fn api_post_json(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .post(format!("{}{}", self.base_url(), path))
            .json(body)
            .send()
            .expect("request should succeed");
        let status = response.status();
        let body = response.json().expect("response body should be json");
        (status, body)
    }

    pub fn api_post_json_with_bearer(
        &self,
        path: &str,
        body: &Value,
        token: &str,
    ) -> (StatusCode, Value) {
        let response = self
            .client
            .post(format!("{}{}", self.base_url(), path))
            .bearer_auth(token)
            .json(body)
            .send()
            .expect("request should succeed");
        let status = response.status();
        let body = response.json().expect("response body should be json");
        (status, body)
    }

    pub fn api_post_stream_with_bearer(&self, path: &str, body: &Value, token: &str) -> Response {
        self.client
            .post(format!("{}{}", self.base_url(), path))
            .bearer_auth(token)
            .json(body)
            .send()
            .expect("stream request should succeed")
    }

    pub fn api_patch_json(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .patch(format!("{}{}", self.base_url(), path))
            .json(body)
            .send()
            .expect("request should succeed");
        let status = response.status();
        let body = response.json().expect("response body should be json");
        (status, body)
    }

    pub fn wait_for_status_up(&mut self) {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let legacy_ready = self
                .client
                .get(format!("{}/status", self.base_url()))
                .send()
                .is_ok_and(|response| response.status().is_success());
            if legacy_ready {
                return;
            }
            // Remote listeners intentionally omit the legacy surface. Health
            // is public, payload-free v2 metadata and therefore a safe probe.
            let v2_ready = self
                .client
                .post(format!(
                    "{}/api/v2/rpc/ddb.api.v2.DdbAdminService/GetHealth",
                    self.base_url()
                ))
                .json(&json!({}))
                .send()
                .is_ok_and(|response| response.status().is_success());
            if v2_ready {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for DDB API health\n{}",
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("child status should be readable")
            {
                self.stopped = true;
                return status;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for DDB process exit\n{}",
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_sessions_len(&mut self, expected: usize) -> Value {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let sessions = self.api_get("/sessions");
            let ready = sessions.as_array().is_some_and(|items| {
                items.len() == expected
                    && items
                        .iter()
                        .all(|session| session["status"].as_str() == Some("ON"))
            });
            if ready {
                return sessions;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {} session(s)\n{}",
                    expected,
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_session_stopped(&mut self, sid: u64) {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let stopped = self
                .api_get("/sessions")
                .as_array()
                .and_then(|sessions| {
                    sessions
                        .iter()
                        .find(|session| session["sid"].as_u64() == Some(sid))
                })
                .is_some_and(|session| {
                    session["status"].as_str() == Some("ON")
                        && session["all_threads_stopped"].as_bool() == Some(true)
                });
            if stopped {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for session {} to become currently stopped\n{}",
                    sid,
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_bkpt_active_sessions(&mut self, bkpt_id: u64, expected: usize) -> Value {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let bkpts = self.api_get("/bkpts");
            if let Some(active_sessions) = bkpts["bkpts"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|bkpt| bkpt["id"].as_u64() == Some(bkpt_id))
                })
                .and_then(|bkpt| bkpt["subbkpts"].as_array())
                .and_then(|subbkpts| {
                    subbkpts.iter().find_map(|subbkpt| {
                        subbkpt["active_sessions"]
                            .as_u64()
                            .map(|value| value as usize)
                    })
                })
            {
                if active_sessions == expected {
                    return bkpts;
                }
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for breakpoint {} to reach {} active session(s)\n{}",
                    bkpt_id,
                    expected,
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_group_id_by_hash(&mut self, hash: &str) -> u64 {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let groups = self.api_get("/groups");
            if let Some(group_id) = groups.as_array().and_then(|items| {
                items
                    .iter()
                    .find(|group| group["hash"] == hash)
                    .and_then(|group| group["id"].as_u64())
            }) {
                return group_id;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for group hash {hash:?}\n{}",
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_stdout_count(&mut self, needle: &str, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let matches = self
                .stdout
                .snapshot()
                .into_iter()
                .filter(|line| line.contains(needle))
                .collect::<Vec<_>>();
            if matches.len() >= expected {
                return matches;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for stdout containing '{}' {} time(s)\n{}",
                    needle,
                    expected,
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_stdout_line(&mut self, needle: &str) -> String {
        self.wait_for_stdout_count(needle, 1)
            .into_iter()
            .next()
            .expect("matching stdout line should exist")
    }

    pub fn wait_for_stdout_line_with_all(&mut self, needles: &[&str]) -> String {
        self.wait_for_stdout_line_with_all_after(0, needles)
    }

    pub fn stdout_checkpoint(&self) -> usize {
        self.stdout.snapshot().len()
    }

    pub fn wait_for_stdout_line_with_all_after(
        &mut self,
        checkpoint: usize,
        needles: &[&str],
    ) -> String {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            if let Some(line) = self
                .stdout
                .snapshot()
                .into_iter()
                .skip(checkpoint)
                .find(|line| needles.iter().all(|needle| line.contains(needle)))
            {
                return line;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for stdout containing all of {:?}\n{}",
                    needles,
                    self.debug_dump()
                );
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn assert_running(&mut self) {
        if let Some(status) = self
            .child
            .try_wait()
            .expect("child status should be readable")
        {
            panic!(
                "ddb exited unexpectedly with status {status}\n{}",
                self.debug_dump()
            );
        }
    }

    fn debug_dump(&self) -> String {
        let stdout = self.stdout.snapshot().join("\n");
        let stderr = self.stderr.snapshot().join("\n");
        let log_dir = self._tempdir.path().join("logs");
        let logs = std::fs::read_dir(&log_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                path.is_file().then(|| {
                    std::fs::read_to_string(&path)
                        .ok()
                        .map(|contents| format!("{}:\n{}", path.display(), contents))
                })?
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "stdout:\n{}\n\nstderr:\n{}\n\nlogs:\n{}",
            stdout, stderr, logs
        )
    }

    fn shutdown(&mut self) {
        if self.stopped {
            return;
        }

        let _ = self.stdin.write_all(b"exit\n");
        let _ = self.stdin.flush();

        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            match self
                .child
                .try_wait()
                .expect("child status should be readable")
            {
                Some(_) => {
                    self.stopped = true;
                    return;
                }
                None if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                None => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    self.stopped = true;
                    return;
                }
            }
        }
    }
}

impl Drop for DdbProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_reader<R>(reader: R, buffer: Arc<OutputBuffer>)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            match line {
                Ok(line) => buffer.push(line),
                Err(_) => break,
            }
        }
    });
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    listener
        .local_addr()
        .expect("local addr should be readable")
        .port()
}

fn render_mock_config(sessions: &[SessionSpec<'_>], exit_on_bootstrap: bool) -> String {
    render_mock_config_with_rejection(sessions, exit_on_bootstrap, "", "")
}

fn render_mock_config_with_rejection(
    sessions: &[SessionSpec<'_>],
    exit_on_bootstrap: bool,
    rejected_tag: &str,
    rejected_command: &str,
) -> String {
    let sessions_yaml = sessions
        .iter()
        .map(|session| {
            let rejection = if session.tag == rejected_tag && !rejected_command.is_empty() {
                format!("\n      reject_commands: [\"{rejected_command}\"]")
            } else {
                String::new()
            };
            format!(
                r#"  - tag: "{tag}"
    alias: "{alias}"
    hash: "{hash}"
    pid: {pid}
    start_delay_ms: {start_delay_ms}
    mock:
      source_file: "{source_file}"
      source_line: {source_line}
      function: "{function}"
      exit_on_continue: {exit_on_continue}
      exit_on_bootstrap: {exit_on_bootstrap}{rejection}"#,
                tag = session.tag,
                alias = session.alias,
                hash = session.hash,
                pid = session.pid,
                start_delay_ms = session.start_delay_ms,
                source_file = session.source_file,
                source_line = session.source_line,
                function = session.function,
                exit_on_continue = session.exit_on_continue,
                exit_on_bootstrap = exit_on_bootstrap,
                rejection = rejection,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Framework: unspecified
Conf:
  auto_shutdown: false
  api_server_port: __API_PORT__
  base_dir: "__BASE_DIR__"
  log_dir: "__LOG_DIR__"
  Debugger:
    backend: mock
StaticSessions:
{sessions_yaml}
"#,
        sessions_yaml = sessions_yaml,
    )
}

fn with_v2_auth(config: String) -> String {
    config.replace(
        "  api_server_port: __API_PORT__",
        "  api_server_port: __API_PORT__\n  api_auth_token_file: \"__API_TOKEN_FILE__\"",
    )
}

fn with_conf_entries(config: String, conf_entries: &str) -> String {
    assert!(
        conf_entries
            .lines()
            .all(|line| line.is_empty() || line.starts_with("  ")),
        "test Conf entries must use two-space YAML indentation"
    );
    config.replacen(
        "  api_server_port: __API_PORT__",
        &format!("  api_server_port: __API_PORT__\n{conf_entries}"),
        1,
    )
}

#[cfg(feature = "grpc-preview")]
fn with_grpc_preview(config: String) -> String {
    config.replace(
        "  api_server_port: __API_PORT__",
        "  api_server_port: __API_PORT__\n  api_grpc_preview_port: __GRPC_PORT__",
    )
}

fn render_real_binary_config(sessions: &[BinarySessionSpec<'_>], backend: &str) -> String {
    let sessions_yaml = sessions
        .iter()
        .map(|session| {
            let args_yaml = session
                .binary_args
                .iter()
                .map(|arg| format!("      - \"{}\"", arg))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                r#"  - tag: "{tag}"
    alias: "{alias}"
    hash: "{hash}"
    pid: {pid}
    ip: "{ip}"
    start_delay_ms: {start_delay_ms}
    start_mode: binary
    binary_path: "{binary_path}"
    stop_at_entry: {stop_at_entry}
    binary_args:
{args_yaml}"#,
                tag = session.tag,
                alias = session.alias,
                hash = session.hash,
                pid = session.pid,
                ip = session.ip,
                start_delay_ms = session.start_delay_ms,
                binary_path = session.binary_path,
                stop_at_entry = session.stop_at_entry,
                args_yaml = args_yaml,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Framework: unspecified
Conf:
  auto_shutdown: false
  on_exit: kill
  api_server_port: __API_PORT__
  base_dir: "__BASE_DIR__"
  log_dir: "__LOG_DIR__"
  Debugger:
    backend: {backend}
StaticSessions:
{sessions_yaml}
"#,
        backend = backend,
        sessions_yaml = sessions_yaml,
    )
}

fn render_real_attach_config(sessions: &[AttachSessionSpec<'_>], backend: &str) -> String {
    let sessions_yaml = sessions
        .iter()
        .map(|session| {
            format!(
                r#"  - tag: "{tag}"
    alias: "{alias}"
    hash: "{hash}"
    pid: {pid}
    ip: "{ip}"
    start_mode: attach"#,
                tag = session.tag,
                alias = session.alias,
                hash = session.hash,
                pid = session.pid,
                ip = session.ip,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Framework: unspecified
Conf:
  auto_shutdown: false
  on_exit: kill
  api_server_port: __API_PORT__
  base_dir: "__BASE_DIR__"
  log_dir: "__LOG_DIR__"
  Debugger:
    backend: {backend}
StaticSessions:
{sessions_yaml}
"#,
        backend = backend,
        sessions_yaml = sessions_yaml,
    )
}

fn render_real_dbt_config(sessions: &[BinarySessionSpec<'_>], backend: &str) -> String {
    let sessions_yaml = sessions
        .iter()
        .map(|session| {
            let args_yaml = session
                .binary_args
                .iter()
                .map(|arg| format!("      - \"{}\"", arg))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                r#"  - tag: "{tag}"
    alias: "{alias}"
    hash: "{hash}"
    pid: {pid}
    ip: "{ip}"
    start_delay_ms: {start_delay_ms}
    start_mode: binary
    binary_path: "{binary_path}"
    stop_at_entry: {stop_at_entry}
    binary_args:
{args_yaml}"#,
                tag = session.tag,
                alias = session.alias,
                hash = session.hash,
                pid = session.pid,
                ip = session.ip,
                start_delay_ms = session.start_delay_ms,
                binary_path = session.binary_path,
                stop_at_entry = session.stop_at_entry,
                args_yaml = args_yaml,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Framework: grpc
Conf:
  auto_shutdown: false
  on_exit: kill
  api_server_port: __API_PORT__
  base_dir: "__BASE_DIR__"
  log_dir: "__LOG_DIR__"
  Debugger:
    backend: {backend}
StaticSessions:
{sessions_yaml}
"#,
        backend = backend,
        sessions_yaml = sessions_yaml,
    )
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn ensure_real_debugger_environment() {
    ensure_debugger_environment("gdb");
}

fn ensure_debugger_environment(debugger: &str) {
    let status = Command::new(debugger)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "{debugger} should be installed and invokable for real integration tests: {error}"
            )
        });
    assert!(
        status.success(),
        "{debugger} --version should succeed for real integration tests"
    );
}

pub fn libfaketime_path() -> PathBuf {
    if let Some(path) = std::env::var_os("LIBFAKETIME_PATH").map(PathBuf::from) {
        assert!(
            path.is_file(),
            "LIBFAKETIME_PATH does not name a file: {}",
            path.display()
        );
        return path;
    }

    let multiarch = match std::env::consts::ARCH {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        architecture => architecture,
    };
    let mut candidates = vec![
        PathBuf::from(format!("/usr/lib/{multiarch}/faketime/libfaketimeMT.so.1")),
        PathBuf::from("/usr/lib/faketime/libfaketimeMT.so.1"),
        PathBuf::from("/usr/local/lib/faketime/libfaketimeMT.so.1"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".local")
                .join("lib")
                .join("faketime")
                .join("libfaketimeMT.so.1"),
        );
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "libfaketimeMT.so.1 should be installed or LIBFAKETIME_PATH should be configured"
            )
        })
}

pub fn build_real_loop_example() -> &'static BuiltRealExample {
    static REAL_LOOP_EXAMPLE: OnceLock<BuiltRealExample> = OnceLock::new();
    REAL_LOOP_EXAMPLE.get_or_init(|| {
        ensure_real_debugger_environment();
        let manifest_path = fixture_root().join("real_loop").join("Cargo.toml");
        let status = Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .status()
            .expect("fixture build command should run");
        assert!(status.success(), "fixture build should succeed");

        let crate_root = manifest_path
            .parent()
            .expect("fixture manifest should have a parent directory");
        let source_path = crate_root.join("src").join("main.rs");
        let binary_path = crate_root
            .join("target")
            .join("debug")
            .join(format!("ddb_real_loop{}", std::env::consts::EXE_SUFFIX));
        assert!(
            binary_path.exists(),
            "fixture binary should exist after build"
        );

        BuiltRealExample {
            manifest_path,
            binary_path,
            source_path: source_path.clone(),
            breakpoint_line: marker_line(&source_path, "BREAKPOINT_MARKER"),
        }
    })
}

pub fn build_real_dbt_example() -> &'static BuiltRealBinaryExample {
    static REAL_DBT_EXAMPLE: OnceLock<BuiltRealBinaryExample> = OnceLock::new();
    REAL_DBT_EXAMPLE.get_or_init(|| {
        ensure_real_debugger_environment();
        let manifest_path = fixture_root().join("real_dbt").join("Cargo.toml");
        let status = Command::new("cargo")
            .arg("build")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .status()
            .expect("fixture build command should run");
        assert!(status.success(), "dbt fixture build should succeed");

        let crate_root = manifest_path
            .parent()
            .expect("fixture manifest should have a parent directory");
        let source_path = crate_root.join("src").join("main.rs");
        let binary_path = crate_root
            .join("target")
            .join("debug")
            .join(format!("DDB{}", std::env::consts::EXE_SUFFIX));
        assert!(
            binary_path.exists(),
            "dbt fixture binary should exist after build"
        );

        BuiltRealBinaryExample {
            manifest_path,
            binary_path,
            source_path,
        }
    })
}

fn marker_line(path: &Path, marker: &str) -> u64 {
    let contents =
        std::fs::read_to_string(path).expect("fixture source should be readable for markers");
    contents
        .lines()
        .enumerate()
        .find_map(|(idx, line)| line.contains(marker).then_some((idx + 1) as u64))
        .expect("fixture marker should exist")
}

pub fn real_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static REAL_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    REAL_TEST_MUTEX
        .get_or_init(|| Mutex::new(()))
        .lock()
        // A failed real-debugger assertion must not turn every later backend test into a
        // misleading lock-poison failure. DDB processes are owned by the test value and are
        // synchronously shut down during unwinding before this lock is released.
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn session_id_by_tag(sessions: &Value, tag: &str) -> u64 {
    sessions
        .as_array()
        .and_then(|items| items.iter().find(|session| session["tag"] == tag))
        .and_then(|session| session["sid"].as_u64())
        .expect("session should exist")
}

pub fn bkpt_id(bkpts: &Value) -> u64 {
    bkpts["bkpts"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|bkpt| bkpt["id"].as_u64())
        .expect("breakpoint should exist")
}

pub fn capture_session_context(ddb: &DdbProcess, sid: u64) -> BTreeMap<String, u64> {
    let (_, names) = ddb.api_post_json(
        "/send",
        &serde_json::json!({
            "wait": true,
            "cmd": format!("-data-list-register-names --session {sid}"),
        }),
    );
    let (_, values) = ddb.api_post_json(
        "/send",
        &serde_json::json!({
            "wait": true,
            "cmd": format!("-data-list-register-values x --session {sid}"),
        }),
    );

    let names_payload = single_response_payload(&names);
    let values_payload = single_response_payload(&values);
    let register_names = encoded_list(&names_payload["register-names"])
        .unwrap_or_else(|| panic!("register-names payload should be a list: {names_payload:?}"));
    let register_values = encoded_list(&values_payload["register-values"])
        .unwrap_or_else(|| panic!("register-values payload should be a list: {values_payload:?}"));

    let values_by_name = register_values
        .iter()
        .filter_map(|entry| {
            let entry = encoded_object(entry)?;
            let number = encoded_field_string(entry, "number")?
                .parse::<usize>()
                .ok()?;
            let value = encoded_field_string(entry, "value")?;
            let name = encoded_string(register_names.get(number)?)?;
            if name.is_empty()
                || !register_alias_names()
                    .iter()
                    .any(|(_, wanted)| name == *wanted)
            {
                None
            } else {
                Some((name.to_string(), parse_register_value(value)))
            }
        })
        .collect::<BTreeMap<_, _>>();

    register_alias_names()
        .iter()
        .filter_map(|(alias, name)| {
            values_by_name
                .get(*name)
                .copied()
                .map(|value| ((*alias).to_string(), value))
        })
        .collect()
}

fn single_response_payload(response: &Value) -> &serde_json::Map<String, Value> {
    response["payload"]["responses"]
        .as_array()
        .and_then(|responses| responses.first())
        .and_then(|response| response["payload"].as_object())
        .expect("response should include a single payload object")
}

fn parse_register_value(value: &str) -> u64 {
    if let Some(hex) = value.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).expect("hex register value should parse successfully");
    }
    value
        .parse::<u64>()
        .expect("register value should parse successfully")
}

fn register_alias_names() -> &'static [(&'static str, &'static str)] {
    #[cfg(target_arch = "x86_64")]
    {
        &[("pc", "rip"), ("sp", "rsp"), ("fp", "rbp")]
    }

    #[cfg(target_arch = "aarch64")]
    {
        &[("pc", "pc"), ("sp", "sp"), ("fp", "x29"), ("lr", "x30")]
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        &[]
    }
}

fn encoded_list(value: &Value) -> Option<&Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get("List").and_then(Value::as_array))
}

fn encoded_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object().map(|object| {
        if let Some(dict) = object.get("Dict").and_then(Value::as_object) {
            dict
        } else {
            object
        }
    })
}

fn encoded_string(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("String").and_then(Value::as_str))
}

fn encoded_field_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<&'a str> {
    object.get(key).and_then(encoded_string)
}
