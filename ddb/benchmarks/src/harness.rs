use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Arc, Mutex, OnceLock},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use tempfile::TempDir;
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

const HTTP_POLL_INTERVAL: Duration = Duration::from_millis(5);
const STDOUT_POLL_INTERVAL: Duration = Duration::from_millis(1);
const DBT_IP: &str = "127.0.0.1";
const DBT_GROUP: &str = "bench-dbt";

#[derive(Default)]
struct OutputBuffer {
    lines: Mutex<Vec<String>>,
}

impl OutputBuffer {
    fn push(&self, line: String) {
        self.lines
            .lock()
            .expect("stdout buffer mutex poisoned")
            .push(line);
    }

    fn line_count(&self) -> usize {
        self.lines
            .lock()
            .expect("stdout buffer mutex poisoned")
            .len()
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .expect("stdout buffer mutex poisoned")
            .clone()
    }

    fn snapshot_from(&self, start_index: usize) -> Vec<String> {
        self.lines
            .lock()
            .expect("stdout buffer mutex poisoned")
            .iter()
            .skip(start_index)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HarnessSpec {
    pub sessions: usize,
    pub threads_per_session: usize,
    pub exit_on_continue: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum RealDebugger {
    Gdb,
    Lldb,
}

impl RealDebugger {
    fn executable(self) -> &'static str {
        match self {
            Self::Gdb => "gdb",
            Self::Lldb => "lldb",
        }
    }

    fn config_name(self) -> &'static str {
        self.executable()
    }
}

impl HarnessSpec {
    pub fn validate(&self) -> Result<()> {
        if self.sessions == 0 {
            bail!("benchmark harness requires at least one session");
        }
        if self.threads_per_session == 0 {
            bail!("benchmark harness requires at least one thread per session");
        }
        Ok(())
    }
}

pub struct DdbHarness {
    _tempdir: TempDir,
    child: Child,
    stdin: ChildStdin,
    stdout: Arc<OutputBuffer>,
    stderr: Arc<OutputBuffer>,
    client: Client,
    port: u16,
    dbt_context_dir: Option<PathBuf>,
    stopped: bool,
}

impl DdbHarness {
    pub fn spawn(binary: &Path, workspace_root: &Path, spec: HarnessSpec) -> Result<Self> {
        spec.validate()?;

        let port = reserve_port()?;
        let tempdir = tempfile::tempdir().context("failed to create temporary benchmark dir")?;
        let config_path = tempdir.path().join("ddb-bench.yaml");
        let state_dir = tempdir.path().join("state");
        let log_dir = tempdir.path().join("logs");
        let config_contents = render_mock_config(spec, port, &state_dir, &log_dir);
        std::fs::write(&config_path, config_contents)
            .context("failed to write benchmark config")?;

        let stdout = Arc::new(OutputBuffer::default());
        let stderr = Arc::new(OutputBuffer::default());

        let mut child = Command::new(binary)
            .arg(&config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(workspace_root.join("core"))
            .spawn()
            .with_context(|| format!("failed to spawn debugger binary {}", binary.display()))?;

        let stdin = child
            .stdin
            .take()
            .context("failed to capture benchmark stdin")?;
        let child_stdout = child
            .stdout
            .take()
            .context("failed to capture benchmark stdout")?;
        let child_stderr = child
            .stderr
            .take()
            .context("failed to capture benchmark stderr")?;

        spawn_reader(child_stdout, Arc::clone(&stdout));
        spawn_reader(child_stderr, Arc::clone(&stderr));

        let client = Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .context("failed to build benchmark http client")?;

        Ok(Self {
            _tempdir: tempdir,
            child,
            stdin,
            stdout,
            stderr,
            client,
            port,
            dbt_context_dir: None,
            stopped: false,
        })
    }

    pub fn spawn_real_dbt(
        binary: &Path,
        workspace_root: &Path,
        debugger: RealDebugger,
        depth: usize,
    ) -> Result<Self> {
        if depth == 0 {
            bail!("distributed backtrace benchmark requires depth >= 1");
        }
        if depth > 16 {
            bail!("distributed backtrace benchmark currently supports depth <= 16");
        }

        ensure_real_debugger_environment(debugger)?;
        let fixture_binary = build_real_dbt_fixture(workspace_root)?;

        let port = reserve_port()?;
        let tempdir = tempfile::tempdir().context("failed to create temporary benchmark dir")?;
        let config_path = tempdir.path().join("ddb-real-dbt-bench.yaml");
        let state_dir = tempdir.path().join("state");
        let log_dir = tempdir.path().join("logs");
        let ctx_dir = tempdir.path().join("dbt-context");
        std::fs::create_dir_all(&ctx_dir).context("failed to create real DBT context directory")?;

        let config_contents = render_real_dbt_config(
            debugger,
            depth,
            &fixture_binary,
            port,
            &state_dir,
            &log_dir,
            &ctx_dir,
        );
        std::fs::write(&config_path, config_contents)
            .context("failed to write real DBT benchmark config")?;

        let stdout = Arc::new(OutputBuffer::default());
        let stderr = Arc::new(OutputBuffer::default());

        let mut child = Command::new(binary)
            .arg(&config_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(workspace_root.join("core"))
            .spawn()
            .with_context(|| format!("failed to spawn debugger binary {}", binary.display()))?;

        let stdin = child
            .stdin
            .take()
            .context("failed to capture benchmark stdin")?;
        let child_stdout = child
            .stdout
            .take()
            .context("failed to capture benchmark stdout")?;
        let child_stderr = child
            .stderr
            .take()
            .context("failed to capture benchmark stderr")?;

        spawn_reader(child_stdout, Arc::clone(&stdout));
        spawn_reader(child_stderr, Arc::clone(&stderr));

        let client = Client::builder()
            .timeout(Duration::from_millis(250))
            .build()
            .context("failed to build benchmark http client")?;

        Ok(Self {
            _tempdir: tempdir,
            child,
            stdin,
            stdout,
            stderr,
            client,
            port,
            dbt_context_dir: Some(ctx_dir),
            stopped: false,
        })
    }

    pub fn wait_for_status_up(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            self.assert_running()?;
            match self
                .client
                .get(format!("http://127.0.0.1:{}/status", self.port))
                .send()
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for /status\n{}", self.debug_dump());
            }
            thread::sleep(HTTP_POLL_INTERVAL);
        }
    }

    pub fn wait_for_sessions_len(&mut self, expected: usize, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            self.assert_running()?;
            let sessions = self.api_get("/sessions");
            match sessions {
                Ok(value) if value.as_array().map(|items| items.len()) == Some(expected) => {
                    return Ok(value);
                }
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for {} sessions\n{}",
                    expected,
                    self.debug_dump()
                );
            }
            thread::sleep(HTTP_POLL_INTERVAL);
        }
    }

    pub fn wait_for_stdout_count(
        &mut self,
        needle: &str,
        expected: usize,
        timeout: Duration,
    ) -> Result<Vec<String>> {
        let deadline = Instant::now() + timeout;
        loop {
            self.assert_running()?;
            let matches = self
                .stdout
                .snapshot()
                .into_iter()
                .filter(|line| line.contains(needle))
                .collect::<Vec<_>>();
            if matches.len() >= expected {
                return Ok(matches);
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for stdout containing `{}` {} time(s)\n{}",
                    needle,
                    expected,
                    self.debug_dump()
                );
            }
            thread::sleep(STDOUT_POLL_INTERVAL);
        }
    }

    pub fn wait_for_notification_subscribers(
        &mut self,
        expected: usize,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            self.assert_running()?;
            let status = self.api_get("/notifications/status");
            match status {
                Ok(value) if value["subscriber_count"].as_u64() == Some(expected as u64) => {
                    return Ok(());
                }
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for {} notification subscribers\n{}",
                    expected,
                    self.debug_dump()
                );
            }
            thread::sleep(HTTP_POLL_INTERVAL);
        }
    }

    pub fn api_get(&self, path: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("http://127.0.0.1:{}{}", self.port, path))
            .send()
            .with_context(|| format!("GET {} failed", path))?;
        let status = response.status();
        if !status.is_success() {
            bail!("GET {} returned {}", path, status);
        }
        response
            .json()
            .with_context(|| format!("GET {} returned invalid json", path))
    }

    pub fn api_post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let response = self
            .client
            .post(format!("http://127.0.0.1:{}{}", self.port, path))
            .json(body)
            .send()
            .with_context(|| format!("POST {} failed", path))?;
        let status = response.status();
        let body_text = response
            .text()
            .with_context(|| format!("POST {} returned unreadable body", path))?;
        if !status.is_success() {
            bail!("POST {} returned {}: {}", path, status, body_text);
        }
        serde_json::from_str(&body_text)
            .with_context(|| format!("POST {} returned invalid json: {}", path, body_text))
    }

    pub fn api_post_json_concurrent(
        &self,
        path: &str,
        bodies: Vec<Value>,
        timeout: Duration,
    ) -> Result<Vec<Value>> {
        let url = format!("http://127.0.0.1:{}{}", self.port, path);
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to build concurrent benchmark client")?;
        bodies
            .into_iter()
            .map(|body| {
                let client = client.clone();
                let url = url.clone();
                let path = path.to_string();
                thread::spawn(move || -> Result<Value> {
                    let response = client
                        .post(url)
                        .json(&body)
                        .send()
                        .with_context(|| format!("POST {} failed", path))?;
                    let status = response.status();
                    let body_text = response
                        .text()
                        .with_context(|| format!("POST {} returned unreadable body", path))?;
                    if !status.is_success() {
                        bail!("POST {} returned {}: {}", path, status, body_text);
                    }
                    serde_json::from_str(&body_text).with_context(|| {
                        format!("POST {} returned invalid json: {}", path, body_text)
                    })
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| anyhow!("concurrent benchmark request panicked"))?
            })
            .collect()
    }

    pub fn send_cli_cmd(&mut self, cmd: &str) -> Result<usize> {
        let cursor = self.stdout.line_count();
        self.stdin
            .write_all(format!("{cmd}\n").as_bytes())
            .with_context(|| format!("failed to write command `{cmd}`"))?;
        self.stdin
            .flush()
            .context("failed to flush benchmark stdin")?;
        Ok(cursor)
    }

    pub fn wait_for_stdout_match<F>(
        &mut self,
        start_index: usize,
        timeout: Duration,
        predicate: F,
    ) -> Result<String>
    where
        F: Fn(&str) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            self.assert_running()?;
            for line in self.stdout.snapshot_from(start_index) {
                if predicate(&line) {
                    return Ok(line);
                }
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for cli output\n{}", self.debug_dump());
            }
            thread::sleep(STDOUT_POLL_INTERVAL);
        }
    }

    pub fn first_group_id(&self) -> Result<u64> {
        let groups = self.api_get("/groups")?;
        groups
            .as_array()
            .and_then(|items| items.first())
            .and_then(|value| value["id"].as_u64())
            .ok_or_else(|| anyhow!("failed to resolve benchmark group id from /groups"))
    }

    pub fn resolve_single_thread_gtid(&mut self, sid: u64, timeout: Duration) -> Result<u64> {
        let token = 70_000 + sid;
        let cursor = self.send_cli_cmd(&format!("{token}-thread-info --session {sid}"))?;
        let line = self.wait_for_stdout_match(cursor, timeout, |line| {
            line.starts_with(&format!("{token}^done"))
        })?;
        extract_first_thread_id(&line)
    }

    pub fn provision_real_dbt_contexts(
        &mut self,
        depth: usize,
        timeout: Duration,
    ) -> Result<Value> {
        let sessions = self.wait_for_sessions_len(depth, timeout)?;

        for role_index in 1..=depth {
            self.wait_for_stdout_count("*stopped", role_index, timeout)?;
            let sid = session_id_by_tag(&sessions, &dbt_session_tag(role_index))?;
            let context = self.capture_session_context(sid)?;
            self.write_dbt_context(role_index, &context)?;
        }

        Ok(sessions)
    }

    fn capture_session_context(&self, sid: u64) -> Result<BTreeMap<String, u64>> {
        let names = self.api_post_json(
            "/send",
            &json!({
                "wait": true,
                "cmd": format!("-data-list-register-names --session {sid}"),
            }),
        )?;
        let values = self.api_post_json(
            "/send",
            &json!({
                "wait": true,
                "cmd": format!("-data-list-register-values x --session {sid}"),
            }),
        )?;

        let names_payload = single_response_payload(&names)?;
        let values_payload = single_response_payload(&values)?;
        let register_names = names_payload
            .get("register-names")
            .and_then(encoded_list)
            .ok_or_else(|| anyhow!("register-names payload missing for session {}", sid))?;
        let register_values = values_payload
            .get("register-values")
            .and_then(encoded_list)
            .ok_or_else(|| anyhow!("register-values payload missing for session {}", sid))?;

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
                    Some((name.to_string(), parse_register_value(value).ok()?))
                }
            })
            .collect::<BTreeMap<_, _>>();

        register_alias_names()
            .iter()
            .map(|(alias, name)| {
                values_by_name
                    .get(*name)
                    .copied()
                    .map(|value| (alias.to_string(), value))
                    .ok_or_else(|| {
                        anyhow!("register {} ({}) missing for session {}", alias, name, sid)
                    })
            })
            .collect()
    }

    fn write_dbt_context(&self, role_index: usize, context: &BTreeMap<String, u64>) -> Result<()> {
        let ctx_dir = self
            .dbt_context_dir
            .as_ref()
            .ok_or_else(|| anyhow!("real DBT context directory not configured"))?;
        let path = ctx_dir.join(format!("ctx-{role_index}.txt"));
        let payload = register_alias_names()
            .iter()
            .filter_map(|(alias, _)| {
                context
                    .get(*alias)
                    .map(|value| format!("{alias}={value}\n"))
            })
            .collect::<String>();
        std::fs::write(&path, payload)
            .with_context(|| format!("failed to write DBT context file {}", path.display()))
    }

    pub fn connect_notification_subscribers(
        &self,
        count: usize,
        timeout: Duration,
    ) -> Result<NotificationSubscribers> {
        let url = format!("ws://127.0.0.1:{}/notifications/subscribe", self.port);
        let mut subscribers = Vec::with_capacity(count);

        for _ in 0..count {
            let (mut socket, _) =
                connect(url.as_str()).with_context(|| format!("failed to connect to {}", url))?;
            set_websocket_timeout(&mut socket, timeout)?;

            let welcome = socket
                .read()
                .context("failed to read websocket welcome message")?;
            let Message::Text(welcome_text) = welcome else {
                bail!("expected websocket welcome text message, got {welcome:?}");
            };
            if !welcome_text.to_string().contains("\"type\":\"welcome\"")
                && !welcome_text.to_string().contains("\"type\": \"welcome\"")
            {
                bail!("unexpected websocket welcome payload: {}", welcome_text);
            }

            let (tx, rx) = mpsc::sync_channel(1);
            let join_handle = thread::spawn(move || {
                let result = loop {
                    match socket.read() {
                        Ok(Message::Text(text)) => {
                            break Ok(text.to_string());
                        }
                        Ok(Message::Ping(_))
                        | Ok(Message::Pong(_))
                        | Ok(Message::Binary(_))
                        | Ok(Message::Frame(_)) => {}
                        Ok(Message::Close(frame)) => {
                            break Err(anyhow!("websocket closed before notification: {frame:?}"));
                        }
                        Err(error) => break Err(anyhow!(error)),
                    }
                };
                let _ = tx.send(result);
            });

            subscribers.push(NotificationSubscriber { rx, join_handle });
        }

        Ok(NotificationSubscribers { subscribers })
    }

    pub fn post_test_notification(&self, message: &str) -> Result<Value> {
        self.api_post_json(
            "/notifications/test",
            &json!({
                "message": message,
            }),
        )
    }

    fn assert_running(&mut self) -> Result<()> {
        if let Some(status) = self
            .child
            .try_wait()
            .context("failed to read benchmark child status")?
        {
            bail!(
                "ddb exited unexpectedly with status {status}\n{}",
                self.debug_dump()
            );
        }
        Ok(())
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

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.stopped = true;
                    return;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(HTTP_POLL_INTERVAL),
                Ok(None) | Err(_) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    self.stopped = true;
                    return;
                }
            }
        }
    }
}

impl Drop for DdbHarness {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct NotificationSubscriber {
    rx: mpsc::Receiver<Result<String>>,
    join_handle: JoinHandle<()>,
}

pub struct NotificationSubscribers {
    subscribers: Vec<NotificationSubscriber>,
}

impl NotificationSubscribers {
    pub fn wait_for_all(self, expected_fragment: &str, timeout: Duration) -> Result<()> {
        for subscriber in self.subscribers {
            let message = subscriber
                .rx
                .recv_timeout(timeout)
                .map_err(|_| anyhow!("timed out waiting for websocket notification"))??;
            if !message.contains(expected_fragment) {
                bail!(
                    "notification payload did not contain expected fragment `{}`: {}",
                    expected_fragment,
                    message
                );
            }
            subscriber
                .join_handle
                .join()
                .expect("notification subscriber thread panicked");
        }
        Ok(())
    }
}

fn render_mock_config(spec: HarnessSpec, port: u16, state_dir: &Path, log_dir: &Path) -> String {
    let sessions_yaml = (0..spec.sessions)
        .map(|index| render_mock_session(spec, index))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Framework: unspecified
Conf:
  auto_shutdown: false
  api_server_port: {port}
  base_dir: "{base_dir}"
  log_dir: "{log_dir}"
  Debugger:
    backend: mock
StaticSessions:
{sessions_yaml}
"#,
        port = port,
        base_dir = state_dir.display(),
        log_dir = log_dir.display(),
        sessions_yaml = sessions_yaml,
    )
}

fn render_real_dbt_config(
    debugger: RealDebugger,
    depth: usize,
    fixture_binary: &Path,
    port: u16,
    state_dir: &Path,
    log_dir: &Path,
    ctx_dir: &Path,
) -> String {
    let binary_path = fixture_binary
        .to_str()
        .expect("fixture binary path should be valid utf-8");
    let sessions_yaml = (1..=depth)
        .map(|role_index| render_real_dbt_session(role_index, binary_path, ctx_dir))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Framework: grpc
Conf:
  auto_shutdown: false
  on_exit: kill
  api_server_port: {port}
  base_dir: "{base_dir}"
  log_dir: "{log_dir}"
  Debugger:
    backend: {backend}
StaticSessions:
{sessions_yaml}
"#,
        backend = debugger.config_name(),
        port = port,
        base_dir = state_dir.display(),
        log_dir = log_dir.display(),
        sessions_yaml = sessions_yaml,
    )
}

fn render_mock_session(spec: HarnessSpec, index: usize) -> String {
    let threads_yaml = (0..spec.threads_per_session)
        .map(|thread_index| {
            let tid = thread_index + 1;
            format!(
                r#"        - id: {tid}
          name: "worker-{tid}""#,
                tid = tid
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"  - tag: "bench-session-{index}"
    alias: "bench-group"
    hash: "bench-hash"
    pid: {pid}
    start_delay_ms: 0
    mock:
      thread_group: "i{group_id}"
      threads:
{threads_yaml}
      source_file: "/bench/source_{index}.rs"
      source_line: {line}
      function: "bench_target_{index}"
      exit_on_continue: {exit_on_continue}"#,
        index = index,
        pid = 10_000 + index as u64,
        group_id = index + 1,
        threads_yaml = threads_yaml,
        line = 100 + index as u64,
        exit_on_continue = spec.exit_on_continue,
    )
}

fn render_real_dbt_session(role_index: usize, binary_path: &str, ctx_dir: &Path) -> String {
    let logical_pid = dbt_logical_pid(role_index);
    let mut args = vec![
        "--logical-pid".to_string(),
        logical_pid.to_string(),
        "--role-index".to_string(),
        role_index.to_string(),
        "--self-ctx-file".to_string(),
        ctx_dir
            .join(format!("ctx-{role_index}.txt"))
            .to_str()
            .expect("context path should be valid utf-8")
            .to_string(),
    ];

    if role_index > 1 {
        args.extend([
            "--parent-ctx-file".to_string(),
            ctx_dir
                .join(format!("ctx-{}.txt", role_index - 1))
                .to_str()
                .expect("parent context path should be valid utf-8")
                .to_string(),
            "--caller-ip".to_string(),
            DBT_IP.to_string(),
            "--caller-pid".to_string(),
            dbt_logical_pid(role_index - 1).to_string(),
            "--caller-tid".to_string(),
            "1".to_string(),
        ]);
    }

    let args_yaml = args
        .iter()
        .map(|arg| format!("      - \"{}\"", arg))
        .collect::<Vec<_>>()
        .join("\n");
    let alias = format!("dbt-{role_index}");

    format!(
        r#"  - tag: "{tag}"
    alias: "{alias}"
    hash: "{hash}"
    pid: {pid}
    ip: "{ip}"
    start_delay_ms: 0
    start_mode: binary
    binary_path: "{binary_path}"
    stop_at_entry: false
    binary_args:
{args_yaml}"#,
        tag = dbt_session_tag(role_index),
        alias = alias,
        hash = DBT_GROUP,
        pid = logical_pid,
        ip = DBT_IP,
        binary_path = binary_path,
        args_yaml = args_yaml,
    )
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

fn reserve_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("failed to bind ephemeral port")?;
    Ok(listener
        .local_addr()
        .context("failed to read ephemeral port")?
        .port())
}

fn ensure_real_debugger_environment(debugger: RealDebugger) -> Result<()> {
    let executable = debugger.executable();
    let status = Command::new(executable)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to invoke {executable} --version"))?;
    if !status.success() {
        bail!("{executable} --version failed");
    }
    Ok(())
}

fn build_real_dbt_fixture(workspace_root: &Path) -> Result<PathBuf> {
    static REAL_DBT_FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    if let Some(path) = REAL_DBT_FIXTURE.get() {
        return Ok(path.clone());
    }

    let manifest_path = workspace_root
        .join("core")
        .join("tests")
        .join("fixtures")
        .join("real_dbt")
        .join("Cargo.toml");
    let status = Command::new("cargo")
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .current_dir(workspace_root)
        .status()
        .context("failed to build real DBT fixture")?;
    if !status.success() {
        bail!("real DBT fixture build failed");
    }

    let binary_path = manifest_path
        .parent()
        .expect("fixture manifest should have a parent directory")
        .join("target")
        .join("debug")
        .join(format!("DDB{}", std::env::consts::EXE_SUFFIX));
    if !binary_path.exists() {
        bail!(
            "real DBT fixture binary does not exist after build: {}",
            binary_path.display()
        );
    }

    let binary_path = binary_path
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", binary_path.display()))?;
    let _ = REAL_DBT_FIXTURE.set(binary_path.clone());
    Ok(binary_path)
}

fn single_response_payload(response: &Value) -> Result<&Map<String, Value>> {
    response
        .get("payload")
        .and_then(|payload| payload.get("responses"))
        .and_then(Value::as_array)
        .and_then(|responses| responses.first())
        .and_then(|response| response.get("payload"))
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("missing single-response payload in {}", response))
}

fn parse_register_value(value: &str) -> Result<u64> {
    if let Some(hex) = value.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16)
            .with_context(|| format!("failed to parse hex register value {}", value));
    }
    value
        .parse::<u64>()
        .with_context(|| format!("failed to parse register value {}", value))
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

fn encoded_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object().map(|object| {
        object
            .get("Dict")
            .and_then(Value::as_object)
            .unwrap_or(object)
    })
}

fn encoded_string(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("String").and_then(Value::as_str))
}

fn encoded_field_string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(encoded_string)
}

pub fn dbt_session_tag(role_index: usize) -> String {
    format!("{DBT_IP}:-{}", dbt_logical_pid(role_index))
}

fn dbt_logical_pid(role_index: usize) -> u64 {
    20_000 + role_index as u64
}

pub fn session_id_by_tag(sessions: &Value, tag: &str) -> Result<u64> {
    sessions
        .as_array()
        .and_then(|items| items.iter().find(|session| session["tag"] == tag))
        .and_then(|session| session["sid"].as_u64())
        .ok_or_else(|| anyhow!("failed to resolve benchmark session for tag {}", tag))
}

fn extract_first_thread_id(line: &str) -> Result<u64> {
    let (_, threads) = line
        .split_once("threads=[")
        .ok_or_else(|| anyhow!("thread-info output missing threads payload: {}", line))?;

    for (offset, _) in threads.match_indices("id=\"") {
        if offset == 0 {
            continue;
        }
        let prefix = threads.as_bytes()[offset - 1];
        if prefix != b'{' && prefix != b',' {
            continue;
        }
        let rest = &threads[offset + 4..];
        let id = rest
            .split('"')
            .next()
            .ok_or_else(|| anyhow!("thread-info output had unterminated id: {}", line))?;
        return id
            .parse::<u64>()
            .with_context(|| format!("invalid thread id in `{}`", line));
    }

    bail!("thread-info output missing thread id: {}", line)
}

fn set_websocket_timeout(
    socket: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    timeout: Duration,
) -> Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            stream
                .set_read_timeout(Some(timeout))
                .context("failed to set websocket read timeout")?;
            stream
                .set_write_timeout(Some(timeout))
                .context("failed to set websocket write timeout")?;
        }
        _ => {
            // The benchmark always connects to ws://127.0.0.1, so the plain stream
            // branch is the hot path. Other variants are left untouched.
        }
    }
    Ok(())
}
