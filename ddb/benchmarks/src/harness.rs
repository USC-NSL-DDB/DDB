use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use tempfile::TempDir;
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

const HTTP_POLL_INTERVAL: Duration = Duration::from_millis(5);
const STDOUT_POLL_INTERVAL: Duration = Duration::from_millis(1);

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
