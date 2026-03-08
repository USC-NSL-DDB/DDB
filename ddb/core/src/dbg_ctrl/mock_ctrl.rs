use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use gdbmi::raw::{Dict, Value};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{sleep, Duration},
};

use crate::{
    common::config::{MockSessionConfig, MockThreadConfig},
    connection::SSHIo,
    dbg_parser::gdb_parser::MIFormatter,
};

use super::{DbgControllable, InputReceiver, OutputSender};

#[derive(Debug, Clone)]
struct MockBreakpoint {
    id: u64,
    location: String,
}

#[derive(Debug)]
struct MockDebuggerState {
    config: MockSessionConfig,
    pid: u64,
    breakpoints: BTreeMap<u64, MockBreakpoint>,
    next_breakpoint_id: u64,
    current_thread_id: u64,
    running: bool,
    bootstrapped: bool,
}

impl MockDebuggerState {
    fn new(config: MockSessionConfig, pid: u64) -> Self {
        let current_thread_id = config.threads.first().map(|thread| thread.id).unwrap_or(1);
        Self {
            config,
            pid,
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            current_thread_id,
            running: false,
            bootstrapped: false,
        }
    }

    fn primary_thread(&self) -> MockThreadConfig {
        self.config
            .threads
            .first()
            .cloned()
            .unwrap_or_else(MockThreadConfig::default)
    }

    fn thread_ids(&self) -> Vec<u64> {
        self.config.threads.iter().map(|thread| thread.id).collect()
    }

    fn frame_payload(&self) -> Dict {
        vec![
            ("addr".to_string(), "0x0000000000401000".into()),
            ("func".to_string(), self.config.function.clone().into()),
            ("file".to_string(), self.config.source_file.clone().into()),
            ("fullname".to_string(), self.config.source_file.clone().into()),
            (
                "line".to_string(),
                self.config.source_line.to_string().into(),
            ),
            ("arch".to_string(), "i386:x86-64".into()),
        ]
        .into()
    }

    fn next_breakpoint(&mut self, location: String) -> MockBreakpoint {
        let bkpt = MockBreakpoint {
            id: self.next_breakpoint_id,
            location,
        };
        self.next_breakpoint_id += 1;
        self.breakpoints.insert(bkpt.id, bkpt.clone());
        bkpt
    }

    fn remove_breakpoint(&mut self, id: u64) {
        self.breakpoints.remove(&id);
    }

    fn current_breakpoint_id(&self) -> Option<u64> {
        self.breakpoints.keys().next().copied()
    }
}

#[derive(Debug)]
pub struct MockAttachController {
    state: Arc<Mutex<MockDebuggerState>>,
    open: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl MockAttachController {
    pub fn new(config: MockSessionConfig, pid: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockDebuggerState::new(config, pid))),
            open: Arc::new(AtomicBool::new(false)),
            task: None,
        }
    }

    async fn send_line(out_tx: &OutputSender, line: String) -> Result<()> {
        out_tx.send_async(Bytes::from(format!("{}\n", line))).await?;
        Ok(())
    }

    async fn send_result(
        out_tx: &OutputSender,
        token: Option<u64>,
        message: &str,
        payload: Option<Dict>,
    ) -> Result<()> {
        let line = MIFormatter::format("^", message, payload.as_ref(), token);
        Self::send_line(out_tx, line).await
    }

    async fn send_notify(
        out_tx: &OutputSender,
        token: Option<u64>,
        message: &str,
        payload: Dict,
    ) -> Result<()> {
        let line = MIFormatter::format("*", message, Some(&payload), token);
        Self::send_line(out_tx, line).await
    }

    async fn send_status(
        out_tx: &OutputSender,
        message: &str,
        payload: Dict,
    ) -> Result<()> {
        let line = MIFormatter::format("=", message, Some(&payload), None);
        Self::send_line(out_tx, line).await
    }

    async fn emit_bootstrap_events(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: OutputSender,
    ) -> Result<()> {
        let state = state.lock().await;
        let tgid = state.config.thread_group.clone();
        let pid = state.pid;
        let threads = state.config.threads.clone();

        drop(state);

        Self::send_status(&out_tx, "thread-group-added", vec![("id".to_string(), tgid.clone().into())].into()).await?;
        Self::send_status(
            &out_tx,
            "thread-group-started",
            vec![
                ("id".to_string(), tgid.clone().into()),
                ("pid".to_string(), pid.to_string().into()),
            ]
            .into(),
        )
        .await?;

        for thread in threads {
            Self::send_status(
                &out_tx,
                "thread-created",
                vec![
                    ("id".to_string(), thread.id.to_string().into()),
                    ("group-id".to_string(), tgid.clone().into()),
                ]
                .into(),
            )
            .await?;
        }
        Ok(())
    }

    fn parse_command(raw: &str) -> (Option<u64>, String, String) {
        let line = raw.trim();
        if line.is_empty() {
            return (None, String::new(), String::new());
        }

        let mut token_end = 0usize;
        for (idx, ch) in line.char_indices() {
            if ch.is_ascii_digit() {
                token_end = idx + ch.len_utf8();
            } else {
                break;
            }
        }
        let (token, command) = if token_end > 0 && line[token_end..].starts_with('-') {
            (line[..token_end].parse::<u64>().ok(), &line[token_end..])
        } else {
            (None, line)
        };

        let mut parts = command.splitn(2, char::is_whitespace);
        let prefix = parts.next().unwrap_or("").trim().to_string();
        let args = parts.next().unwrap_or("").trim().to_string();
        (token, prefix, args)
    }

    fn thread_info_payload(state: &MockDebuggerState, requested_tid: Option<u64>) -> Dict {
        let threads = state
            .config
            .threads
            .iter()
            .filter(|thread| requested_tid.map(|tid| tid == thread.id).unwrap_or(true))
            .map(|thread| {
                let mut thread_payload: Dict = vec![
                    ("id".to_string(), thread.id.to_string().into()),
                    (
                        "target-id".to_string(),
                        format!("Thread {}", thread.id).into(),
                    ),
                    ("name".to_string(), thread.name.clone().into()),
                    (
                        "state".to_string(),
                        if state.running { "running" } else { "stopped" }.into(),
                    ),
                ]
                .into();
                thread_payload.insert(
                    "frame".to_string(),
                    Value::Dict(state.frame_payload()),
                );
                Value::Dict(thread_payload)
            })
            .collect::<Vec<_>>();

        vec![
            ("threads".to_string(), Value::List(threads)),
            (
                "current-thread-id".to_string(),
                state.current_thread_id.to_string().into(),
            ),
        ]
        .into()
    }

    fn list_thread_groups_payload(state: &MockDebuggerState) -> Dict {
        let executable = if state.config.executable.is_empty() {
            format!("/mock/bin/{}", state.config.function)
        } else {
            state.config.executable.clone()
        };
        vec![(
            "groups".to_string(),
            Value::List(vec![Value::Dict(
                vec![
                    ("id".to_string(), state.config.thread_group.clone().into()),
                    ("type".to_string(), "process".into()),
                    ("pid".to_string(), state.pid.to_string().into()),
                    ("executable".to_string(), executable.into()),
                ]
                .into(),
            )]),
        )]
        .into()
    }

    fn breakpoint_payload(bkpt: &MockBreakpoint) -> Dict {
        vec![(
            "bkpt".to_string(),
            Value::Dict(
                vec![
                    ("number".to_string(), bkpt.id.to_string().into()),
                    ("type".to_string(), "breakpoint".into()),
                    ("disp".to_string(), "keep".into()),
                    ("enabled".to_string(), "y".into()),
                    ("times".to_string(), "0".into()),
                    (
                        "original-location".to_string(),
                        bkpt.location.clone().into(),
                    ),
                ]
                .into(),
            ),
        )]
        .into()
    }

    async fn schedule_continue_stop(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: OutputSender,
        token: Option<u64>,
    ) {
        sleep(Duration::from_millis(25)).await;
        let (payload, should_emit) = {
            let mut state = state.lock().await;
            state.running = false;
            if state.config.exit_on_continue {
                (
                    vec![
                        ("reason".to_string(), "exited-normally".into()),
                        (
                            "thread-id".to_string(),
                            state.current_thread_id.to_string().into(),
                        ),
                        ("stopped-threads".to_string(), "all".into()),
                    ]
                    .into(),
                    true,
                )
            } else {
                let mut payload: Dict = vec![
                    (
                        "reason".to_string(),
                        if state.current_breakpoint_id().is_some() {
                            "breakpoint-hit"
                        } else {
                            "end-stepping-range"
                        }
                        .into(),
                    ),
                    (
                        "thread-id".to_string(),
                        state.current_thread_id.to_string().into(),
                    ),
                    ("stopped-threads".to_string(), "all".into()),
                    ("frame".to_string(), Value::Dict(state.frame_payload())),
                ]
                .into();
                if let Some(bkpt_id) = state.current_breakpoint_id() {
                    payload.insert("bkptno".to_string(), bkpt_id.to_string().into());
                }
                (payload, true)
            }
        };

        if should_emit {
            let _ = Self::send_notify(&out_tx, token, "stopped", payload).await;
        }
    }

    async fn schedule_interrupt_stop(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: OutputSender,
        token: Option<u64>,
    ) {
        sleep(Duration::from_millis(10)).await;
        let payload = {
            let mut state = state.lock().await;
            state.running = false;
            vec![
                ("reason".to_string(), "signal-received".into()),
                (
                    "thread-id".to_string(),
                    state.current_thread_id.to_string().into(),
                ),
                ("stopped-threads".to_string(), "all".into()),
                ("frame".to_string(), Value::Dict(state.frame_payload())),
            ]
            .into()
        };
        let _ = Self::send_notify(&out_tx, token, "stopped", payload).await;
    }

    async fn handle_command(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: OutputSender,
        token: Option<u64>,
        prefix: String,
        args: String,
    ) -> Result<()> {
        match prefix.as_str() {
            "" => {}
            "-mock-bootstrap" => {
                let should_bootstrap = {
                    let mut state = state.lock().await;
                    let should_bootstrap = !state.bootstrapped;
                    state.bootstrapped = true;
                    should_bootstrap
                };
                Self::send_result(&out_tx, token, "done", None).await?;
                if should_bootstrap {
                    Self::emit_bootstrap_events(state, out_tx).await?;
                }
            }
            "-thread-info" => {
                let requested_tid = args
                    .split_whitespace()
                    .next()
                    .and_then(|value| value.parse::<u64>().ok());
                let payload = {
                    let state = state.lock().await;
                    Self::thread_info_payload(&state, requested_tid)
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-list-thread-groups" => {
                let payload = {
                    let state = state.lock().await;
                    Self::list_thread_groups_payload(&state)
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-thread-select" => {
                let new_thread_id = args
                    .split_whitespace()
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1);
                {
                    let mut state = state.lock().await;
                    state.current_thread_id = new_thread_id;
                }
                let payload: Dict =
                    vec![("new-thread-id".to_string(), new_thread_id.to_string().into())].into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-break-insert" => {
                let location = args
                    .split_whitespace()
                    .last()
                    .unwrap_or("main.rs:1")
                    .trim_matches(['"', '\''])
                    .to_string();
                let bkpt = {
                    let mut state = state.lock().await;
                    state.next_breakpoint(location)
                };
                Self::send_result(
                    &out_tx,
                    token,
                    "done",
                    Some(Self::breakpoint_payload(&bkpt)),
                )
                .await?;
            }
            "-break-delete" => {
                if let Some(local_id) = args
                    .split_whitespace()
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    let mut state = state.lock().await;
                    state.remove_breakpoint(local_id);
                }
                Self::send_result(&out_tx, token, "done", None).await?;
            }
            "-exec-continue" => {
                let thread_ids = {
                    let mut state = state.lock().await;
                    state.running = true;
                    state.thread_ids()
                };
                Self::send_result(&out_tx, token, "running", None).await?;
                let running_payload: Dict = vec![("thread-id".to_string(), "all".into())].into();
                Self::send_notify(&out_tx, token, "running", running_payload).await?;
                if thread_ids.is_empty() {
                    return Ok(());
                }
                tokio::spawn(Self::schedule_continue_stop(state, out_tx, token));
            }
            "-exec-interrupt-if-running" | "-exec-interrupt" => {
                let should_interrupt = {
                    let state = state.lock().await;
                    state.running
                };
                Self::send_result(&out_tx, token, "done", None).await?;
                if should_interrupt {
                    tokio::spawn(Self::schedule_interrupt_stop(state, out_tx, token));
                }
            }
            "detach" | "kill" | "exit" => {
                Self::send_result(&out_tx, token, "done", None).await?;
            }
            _ => {
                Self::send_result(&out_tx, token, "done", None).await?;
            }
        }
        Ok(())
    }

    async fn run(
        state: Arc<Mutex<MockDebuggerState>>,
        in_rx: InputReceiver,
        out_tx: OutputSender,
        open: Arc<AtomicBool>,
    ) {
        while let Ok(data) = in_rx.recv_async().await {
            let Ok(commands) = std::str::from_utf8(data.as_ref()) else {
                continue;
            };
            for raw_cmd in commands.lines() {
                let (token, prefix, args) = Self::parse_command(raw_cmd);
                if prefix.is_empty() && args.is_empty() {
                    continue;
                }
                if Self::handle_command(
                    Arc::clone(&state),
                    out_tx.clone(),
                    token,
                    prefix,
                    args,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }
        open.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl DbgControllable for MockAttachController {
    type InputType = Bytes;

    async fn start(&mut self, _cmd: &str) -> Result<SSHIo> {
        let (in_tx, in_rx) = flume::bounded::<Bytes>(1024);
        let (out_tx, out_rx) = flume::bounded::<Bytes>(1024);
        self.open.store(true, Ordering::SeqCst);
        self.task = Some(tokio::spawn(Self::run(
            Arc::clone(&self.state),
            in_rx,
            out_tx,
            Arc::clone(&self.open),
        )));
        Ok(SSHIo { in_tx, out_rx })
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    async fn close(&mut self) -> Result<()> {
        self.open.store(false, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        Ok(())
    }
}
