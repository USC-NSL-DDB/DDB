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

use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde_json::Value;
use tempfile::TempDir;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    stopped: bool,
}

impl DdbProcess {
    pub fn spawn(sessions: &[SessionSpec<'_>]) -> Self {
        let config_contents = render_mock_config(sessions, false);
        Self::spawn_with_config("ddb-integration.yaml", config_contents)
    }

    pub fn spawn_with_bootstrap_exit(sessions: &[SessionSpec<'_>]) -> Self {
        let config_contents = render_mock_config(sessions, true);
        Self::spawn_with_config("ddb-bootstrap-exit-integration.yaml", config_contents)
    }

    pub fn spawn_real_binary_sessions(sessions: &[BinarySessionSpec<'_>]) -> Self {
        let config_contents = render_real_binary_config(sessions);
        Self::spawn_with_config("ddb-real-integration.yaml", config_contents)
    }

    pub fn spawn_real_dbt_sessions(sessions: &[BinarySessionSpec<'_>]) -> Self {
        let config_contents = render_real_dbt_config(sessions);
        Self::spawn_with_config("ddb-real-dbt-integration.yaml", config_contents)
    }

    fn spawn_with_config(config_name: &str, config_contents: String) -> Self {
        let port = reserve_port();
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = tempdir.path().join(config_name);
        let state_dir = tempdir.path().join("state");
        let log_dir = tempdir.path().join("logs");
        let config_contents = config_contents
            .replace("__API_PORT__", &port.to_string())
            .replace(
                "__BASE_DIR__",
                state_dir.to_str().expect("state dir should be valid utf-8"),
            )
            .replace(
                "__LOG_DIR__",
                log_dir.to_str().expect("log dir should be valid utf-8"),
            );
        std::fs::write(&config_path, config_contents).expect("config file should be written");

        let stdout = Arc::new(OutputBuffer::default());
        let stderr = Arc::new(OutputBuffer::default());
        let binary = std::env::var("CARGO_BIN_EXE_ddb")
            .ok()
            .or_else(|| option_env!("CARGO_BIN_EXE_ddb").map(str::to_string))
            .expect("ddb binary path should be set");

        let mut child = Command::new(binary)
            .arg(config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .spawn()
            .expect("ddb should spawn");

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
            stopped: false,
        };
        process.wait_for_status_up();
        process
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
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

    pub fn wait_for_status_up(&mut self) {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            match self
                .client
                .get(format!("{}/status", self.base_url()))
                .send()
            {
                Ok(response) if response.status().is_success() => return,
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for /status\n{}", self.debug_dump());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    pub fn wait_for_sessions_len(&mut self, expected: usize) -> Value {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            let sessions = self.api_get("/sessions");
            if sessions.as_array().map(|items| items.len()) == Some(expected) {
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
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            if let Some(line) = self
                .stdout
                .snapshot()
                .into_iter()
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
        format!("stdout:\n{}\n\nstderr:\n{}", stdout, stderr)
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
    let sessions_yaml = sessions
        .iter()
        .map(|session| {
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
      exit_on_bootstrap: {exit_on_bootstrap}"#,
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

fn render_real_binary_config(sessions: &[BinarySessionSpec<'_>]) -> String {
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
    backend: gdb
StaticSessions:
{sessions_yaml}
"#,
        sessions_yaml = sessions_yaml,
    )
}

fn render_real_dbt_config(sessions: &[BinarySessionSpec<'_>]) -> String {
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
    backend: gdb
StaticSessions:
{sessions_yaml}
"#,
        sessions_yaml = sessions_yaml,
    )
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn ensure_real_debugger_environment() {
    let status = Command::new("gdb")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("gdb should be installed and invokable for real integration tests");
    assert!(
        status.success(),
        "gdb --version should succeed for real integration tests"
    );
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
        .expect("real test mutex should not be poisoned")
}

pub fn session_id_by_tag(sessions: &Value, tag: &str) -> u64 {
    sessions
        .as_array()
        .and_then(|items| items.iter().find(|session| session["tag"] == tag))
        .and_then(|session| session["sid"].as_u64())
        .expect("session should exist")
}

pub fn group_id_by_hash(groups: &Value, hash: &str) -> u64 {
    groups
        .as_array()
        .and_then(|items| items.iter().find(|group| group["hash"] == hash))
        .and_then(|group| group["id"].as_u64())
        .expect("group should exist")
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
