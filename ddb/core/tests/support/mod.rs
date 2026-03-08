#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
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
        let port = reserve_port();
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = tempdir.path().join("ddb-integration.yaml");
        std::fs::write(
            &config_path,
            render_config(
                port,
                tempdir
                    .path()
                    .join("state")
                    .to_str()
                    .expect("state dir should be valid utf-8"),
                tempdir
                    .path()
                    .join("logs")
                    .to_str()
                    .expect("log dir should be valid utf-8"),
                sessions,
            ),
        )
        .expect("config file should be written");

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

    pub fn wait_for_status_up(&mut self) {
        let deadline = Instant::now() + DEFAULT_TIMEOUT;
        loop {
            self.assert_running();
            match self.client.get(format!("{}/status", self.base_url())).send() {
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
                .and_then(|items| items.iter().find(|bkpt| bkpt["id"].as_u64() == Some(bkpt_id)))
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
        if let Some(status) = self.child.try_wait().expect("child status should be readable") {
            panic!("ddb exited unexpectedly with status {status}\n{}", self.debug_dump());
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
            match self.child.try_wait().expect("child status should be readable") {
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

fn render_config(port: u16, base_dir: &str, log_dir: &str, sessions: &[SessionSpec<'_>]) -> String {
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
      exit_on_continue: {exit_on_continue}"#,
                tag = session.tag,
                alias = session.alias,
                hash = session.hash,
                pid = session.pid,
                start_delay_ms = session.start_delay_ms,
                source_file = session.source_file,
                source_line = session.source_line,
                function = session.function,
                exit_on_continue = session.exit_on_continue,
            )
        })
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
        base_dir = base_dir,
        log_dir = log_dir,
        sessions_yaml = sessions_yaml,
    )
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
