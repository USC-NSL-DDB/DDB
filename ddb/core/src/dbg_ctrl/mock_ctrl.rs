use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::debugger::protocol::{Dict, Value};
use anyhow::{bail, Result};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{sleep, Duration},
};

use crate::{
    cmd_flow::mi::MiFormatter,
    common::mock_fixture::{MockDbtParentConfig, MockSessionConfig, MockStackFrameConfig},
    connection::{RunningTransport, TransportEvent, TransportRequest},
};

use super::DebuggerTransport;

type OutputSender = flume::Sender<TransportEvent>;
const MAX_MOCK_VARIABLES_PER_FRAME: usize = 10_000;

#[derive(Debug, Clone)]
struct MockBreakpoint {
    id: u64,
    location: String,
    enabled: bool,
    condition: Option<String>,
}

#[derive(Debug)]
struct MockDebuggerState {
    config: MockSessionConfig,
    pid: u64,
    breakpoints: BTreeMap<u64, MockBreakpoint>,
    next_breakpoint_id: u64,
    current_thread_id: u64,
    current_context_regs: BTreeMap<String, u64>,
    current_source_line: u64,
    running: bool,
    bootstrapped: bool,
}

impl MockDebuggerState {
    fn new(config: MockSessionConfig, pid: u64) -> Self {
        let current_thread_id = config.threads.first().map(|thread| thread.id).unwrap_or(1);
        let current_source_line = config
            .stack_frames
            .first()
            .map(|frame| frame.line)
            .unwrap_or(config.source_line);
        Self {
            current_context_regs: config.context_regs.clone(),
            current_source_line,
            config,
            pid,
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            current_thread_id,
            running: false,
            bootstrapped: false,
        }
    }

    fn thread_ids(&self) -> Vec<u64> {
        self.config.threads.iter().map(|thread| thread.id).collect()
    }

    fn frame_payload(&self) -> Dict {
        let (function, source_file) = self
            .config
            .stack_frames
            .first()
            .map(|frame| (frame.function.as_str(), frame.file.as_str()))
            .unwrap_or((&self.config.function, &self.config.source_file));
        let address = self
            .current_context_regs
            .get("pc")
            .copied()
            .unwrap_or(0x401000);
        vec![
            ("addr".to_string(), format!("0x{address:016x}").into()),
            ("func".to_string(), function.to_string().into()),
            ("file".to_string(), source_file.to_string().into()),
            ("fullname".to_string(), source_file.to_string().into()),
            (
                "line".to_string(),
                self.current_source_line.to_string().into(),
            ),
            ("arch".to_string(), "i386:x86-64".into()),
        ]
        .into()
    }

    fn stack_frames(&self) -> &[MockStackFrameConfig] {
        if self.config.stack_frames.is_empty() {
            &[]
        } else {
            &self.config.stack_frames
        }
    }

    fn stack_frames_payload(&self) -> Dict {
        let frames = if self.stack_frames().is_empty() {
            let mut frame = self.frame_payload();
            frame.insert("level".to_string(), "0".into());
            vec![Value::Dict(frame)]
        } else {
            self.stack_frames()
                .iter()
                .enumerate()
                .map(|(idx, frame)| {
                    let line = if idx == 0 {
                        self.current_source_line
                    } else {
                        frame.line
                    };
                    Value::Dict(
                        vec![
                            ("level".to_string(), idx.to_string().into()),
                            (
                                "addr".to_string(),
                                format!("0x{:016x}", 0x401000_u64 + idx as u64 * 0x10).into(),
                            ),
                            ("func".to_string(), frame.function.clone().into()),
                            ("file".to_string(), frame.file.clone().into()),
                            ("fullname".to_string(), frame.file.clone().into()),
                            ("line".to_string(), line.to_string().into()),
                            ("arch".to_string(), "i386:x86-64".into()),
                        ]
                        .into(),
                    )
                })
                .collect()
        };

        vec![("stack".to_string(), Value::List(frames))].into()
    }

    fn stack_variables_payload(&self) -> Dict {
        let variables = (0..self.config.variables_per_frame)
            .map(|index| {
                let (name, type_name, value, children) = match index {
                    0 => ("counter".to_string(), "uint64_t", "42".to_string(), 0),
                    1 => ("request".to_string(), "Request *", "0x1000".to_string(), 3),
                    _ => (
                        format!("bench_variable_{index}"),
                        "uint64_t",
                        index.to_string(),
                        0,
                    ),
                };
                Value::Dict(
                    vec![
                        ("name".to_string(), name.into()),
                        ("type".to_string(), type_name.into()),
                        ("value".to_string(), value.into()),
                        ("numchild".to_string(), children.to_string().into()),
                    ]
                    .into(),
                )
            })
            .collect();
        vec![("variables".to_string(), Value::List(variables))].into()
    }

    fn dbt_payload(&self) -> Dict {
        match &self.config.dbt_parent {
            Some(parent) => {
                let caller_ctx: Dict = parent
                    .caller_ctx
                    .iter()
                    .map(|(reg, value)| (reg.clone(), value.to_string().into()))
                    .collect::<Vec<_>>()
                    .into();
                let caller_meta = Self::dbt_parent_meta(parent);

                vec![
                    ("message".to_string(), "success".into()),
                    (
                        "metadata".to_string(),
                        Value::Dict(
                            vec![
                                ("caller_ctx".to_string(), Value::Dict(caller_ctx)),
                                ("caller_meta".to_string(), Value::Dict(caller_meta)),
                                (
                                    "local_meta".to_string(),
                                    Value::Dict(
                                        vec![(
                                            "tid".to_string(),
                                            self.current_thread_id.to_string().into(),
                                        )]
                                        .into(),
                                    ),
                                ),
                            ]
                            .into(),
                        ),
                    ),
                ]
                .into()
            }
            None => vec![("message".to_string(), "failed".into())].into(),
        }
    }

    fn dbt_parent_meta(parent: &MockDbtParentConfig) -> Dict {
        vec![
            ("ip".to_string(), u32::from(parent.ip).to_string().into()),
            ("pid".to_string(), parent.pid.to_string().into()),
            ("tid".to_string(), parent.tid.to_string().into()),
            (
                "proclet_id".to_string(),
                if parent.proclet_id.is_empty() {
                    "0".into()
                } else {
                    parent.proclet_id.clone().into()
                },
            ),
        ]
        .into()
    }

    fn switch_context(&mut self, args: &str) -> Dict {
        let old_ctx: Dict = self
            .current_context_regs
            .iter()
            .map(|(reg, value)| (reg.clone(), value.to_string().into()))
            .collect::<Vec<_>>()
            .into();

        for reg_pair in args.split_whitespace() {
            if let Some((reg, value)) = reg_pair.split_once('=') {
                if let Ok(value) = value.parse::<u64>() {
                    self.current_context_regs.insert(reg.to_string(), value);
                }
            }
        }

        vec![
            ("message".to_string(), "success".into()),
            ("old_ctx".to_string(), Value::Dict(old_ctx)),
        ]
        .into()
    }

    fn next_breakpoint(&mut self, location: String, enabled: bool) -> MockBreakpoint {
        let bkpt = MockBreakpoint {
            id: self.next_breakpoint_id,
            location,
            enabled,
            condition: None,
        };
        self.next_breakpoint_id += 1;
        self.breakpoints.insert(bkpt.id, bkpt.clone());
        bkpt
    }

    fn remove_breakpoint(&mut self, id: u64) {
        self.breakpoints.remove(&id);
    }

    fn current_breakpoint_id(&self) -> Option<u64> {
        self.breakpoints
            .values()
            .find(|breakpoint| breakpoint.enabled)
            .map(|breakpoint| breakpoint.id)
    }

    fn advance_source_line(&mut self) {
        self.current_source_line = self.current_source_line.saturating_add(1);
        if let Some(pc) = self.current_context_regs.get_mut("pc") {
            *pc = pc.saturating_add(4);
        }
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
        out_tx
            .send_async(TransportEvent::Stdout(Bytes::from(format!("{}\n", line))))
            .await?;
        Ok(())
    }

    async fn send_stream(out_tx: &OutputSender, stream: &str, message: &str) -> Result<()> {
        Self::send_line(out_tx, MiFormatter::format_stream(stream, message)).await
    }

    async fn send_result(
        out_tx: &OutputSender,
        token: Option<u64>,
        message: &str,
        payload: Option<Dict>,
    ) -> Result<()> {
        let line = MiFormatter::format("^", message, payload.as_ref(), token);
        Self::send_line(out_tx, line).await
    }

    async fn send_notify(
        out_tx: &OutputSender,
        token: Option<u64>,
        message: &str,
        payload: Dict,
    ) -> Result<()> {
        let line = MiFormatter::format("*", message, Some(&payload), token);
        Self::send_line(out_tx, line).await
    }

    async fn send_status(out_tx: &OutputSender, message: &str, payload: Dict) -> Result<()> {
        let line = MiFormatter::format("=", message, Some(&payload), None);
        Self::send_line(out_tx, line).await
    }

    fn mock_output_stream(args: &str) -> (usize, String) {
        let mut arguments = args.split_whitespace();
        let count = arguments
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 4_096);
        let bytes = arguments
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or("mock console output\n".len())
            .clamp(1, 64 * 1024);
        let message = if args.is_empty() {
            "mock console output\n".to_string()
        } else {
            let mut message = "x".repeat(bytes.saturating_sub(1));
            message.push('\n');
            message
        };
        (count, message)
    }

    async fn emit_bootstrap_events(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: flume::Sender<TransportEvent>,
    ) -> Result<()> {
        let state = state.lock().await;
        let tgid = state.config.thread_group.clone();
        let pid = state.pid;
        let threads = state.config.threads.clone();
        let current_thread_id = state.current_thread_id;
        let frame = state.frame_payload();

        drop(state);

        Self::send_status(
            &out_tx,
            "thread-group-added",
            vec![("id".to_string(), tgid.clone().into())].into(),
        )
        .await?;
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
        Self::send_notify(
            &out_tx,
            None,
            "stopped",
            vec![
                ("reason".to_string(), "breakpoint-hit".into()),
                (
                    "thread-id".to_string(),
                    current_thread_id.to_string().into(),
                ),
                ("stopped-threads".to_string(), "all".into()),
                ("frame".to_string(), Value::Dict(frame)),
            ]
            .into(),
        )
        .await?;
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
                thread_payload.insert("frame".to_string(), Value::Dict(state.frame_payload()));
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

    fn source_files_payload(state: &MockDebuggerState) -> Dict {
        vec![(
            "files".to_string(),
            Value::List(vec![Value::Dict(
                vec![
                    ("file".to_string(), state.config.source_file.clone().into()),
                    (
                        "fullname".to_string(),
                        state.config.source_file.clone().into(),
                    ),
                ]
                .into(),
            )]),
        )]
        .into()
    }

    fn breakpoint_details(bkpt: &MockBreakpoint) -> Dict {
        let mut details: Dict = vec![
            ("number".to_string(), bkpt.id.to_string().into()),
            ("type".to_string(), "breakpoint".into()),
            ("disp".to_string(), "keep".into()),
            (
                "enabled".to_string(),
                if bkpt.enabled { "y" } else { "n" }.into(),
            ),
            ("times".to_string(), "0".into()),
            (
                "original-location".to_string(),
                bkpt.location.clone().into(),
            ),
        ]
        .into();
        if let Some(condition) = &bkpt.condition {
            details.insert("cond".to_string(), condition.clone().into());
        }
        details
    }

    fn breakpoint_payload(bkpt: &MockBreakpoint) -> Dict {
        vec![(
            "bkpt".to_string(),
            Value::Dict(Self::breakpoint_details(bkpt)),
        )]
        .into()
    }

    fn breakpoint_table_payload(state: &MockDebuggerState) -> Dict {
        let body = state
            .breakpoints
            .values()
            .map(|breakpoint| {
                Value::Dict(
                    vec![(
                        "bkpt".to_string(),
                        Value::Dict(Self::breakpoint_details(breakpoint)),
                    )]
                    .into(),
                )
            })
            .collect::<Vec<_>>();
        vec![(
            "BreakpointTable".to_string(),
            Value::Dict(
                vec![
                    ("nr_rows".to_string(), body.len().to_string().into()),
                    ("nr_cols".to_string(), "6".into()),
                    ("hdr".to_string(), Value::List(Vec::new())),
                    ("body".to_string(), Value::List(body)),
                ]
                .into(),
            ),
        )]
        .into()
    }

    async fn schedule_continue_stop(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: flume::Sender<TransportEvent>,
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
        out_tx: flume::Sender<TransportEvent>,
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

    async fn schedule_step_stop(
        state: Arc<Mutex<MockDebuggerState>>,
        out_tx: flume::Sender<TransportEvent>,
        token: Option<u64>,
    ) {
        sleep(Duration::from_millis(25)).await;
        let payload = {
            let mut state = state.lock().await;
            state.running = false;
            state.advance_source_line();
            vec![
                ("reason".to_string(), "end-stepping-range".into()),
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
        out_tx: flume::Sender<TransportEvent>,
        token: Option<u64>,
        prefix: String,
        args: String,
    ) -> Result<()> {
        let should_reject = {
            let state = state.lock().await;
            state
                .config
                .reject_commands
                .iter()
                .any(|command| command == &prefix)
        };
        if should_reject {
            let payload: Dict = vec![(
                "msg".to_string(),
                "configured mock command rejection".into(),
            )]
            .into();
            Self::send_result(&out_tx, token, "error", Some(payload)).await?;
            return Ok(());
        }
        match prefix.as_str() {
            "" => {}
            "-mock-bootstrap" => {
                let (should_bootstrap, exit_on_bootstrap) = {
                    let mut state = state.lock().await;
                    let should_bootstrap = !state.bootstrapped;
                    state.bootstrapped = true;
                    (should_bootstrap, state.config.exit_on_bootstrap)
                };
                Self::send_result(&out_tx, token, "done", None).await?;
                if should_bootstrap && exit_on_bootstrap {
                    out_tx.send_async(TransportEvent::Exited(Some(0))).await?;
                } else if should_bootstrap {
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
            "-file-list-exec-source-files" => {
                let payload = {
                    let state = state.lock().await;
                    Self::source_files_payload(&state)
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-stack-list-frames" => {
                let payload = {
                    let state = state.lock().await;
                    state.stack_frames_payload()
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-stack-list-variables" => {
                let payload = {
                    let state = state.lock().await;
                    state.stack_variables_payload()
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-list-signals" => {
                let signal =
                    |name: &str, stop: &str, print: &str, pass: &str, description: &str| {
                        Value::Dict(
                            vec![
                                ("name".to_string(), name.into()),
                                ("stop".to_string(), stop.into()),
                                ("print".to_string(), print.into()),
                                ("pass".to_string(), pass.into()),
                                ("description".to_string(), description.into()),
                            ]
                            .into(),
                        )
                    };
                let payload: Dict = vec![(
                    "signals".to_string(),
                    Value::List(vec![
                        signal("SIGINT", "Yes", "Yes", "No", "Interrupt"),
                        signal("SIGTERM", "Yes", "Yes", "Yes", "Terminated"),
                        signal("SIGUSR1", "No", "Yes", "Yes", "User signal 1"),
                    ]),
                )]
                .into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }

            "-data-list-register-names" => {
                let payload: Dict = vec![(
                    "register-names".to_string(),
                    Value::List(vec!["rax".into(), "rsp".into(), "rip".into()]),
                )]
                .into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-data-list-register-values" => {
                let parts = args.split_whitespace().collect::<Vec<_>>();
                let format_index = parts
                    .iter()
                    .position(|part| matches!(*part, "N" | "x" | "d" | "t"));
                let format = format_index
                    .and_then(|index| parts.get(index))
                    .copied()
                    .unwrap_or("N");
                let requested = format_index
                    .map(|index| {
                        parts[index + 1..]
                            .iter()
                            .filter_map(|part| part.parse::<usize>().ok())
                            .collect::<Vec<_>>()
                    })
                    .filter(|values| !values.is_empty())
                    .unwrap_or_else(|| vec![0, 1, 2]);
                let raw_values = [42_u64, 0x1000, 0x1010];
                let values = requested
                    .into_iter()
                    .filter_map(|number| {
                        let value = *raw_values.get(number)?;
                        let rendered = match format {
                            "x" => format!("0x{value:x}"),
                            "d" => value.to_string(),
                            "t" => format!("0b{value:b}"),
                            _ => match number {
                                0 => value.to_string(),
                                _ => format!("0x{value:x}"),
                            },
                        };
                        Some(Value::Dict(
                            vec![
                                ("number".to_string(), number.to_string().into()),
                                ("value".to_string(), rendered.into()),
                            ]
                            .into(),
                        ))
                    })
                    .collect();
                let payload: Dict =
                    vec![("register-values".to_string(), Value::List(values))].into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-var-create" => {
                let parts = args.split_whitespace().collect::<Vec<_>>();
                let marker = parts
                    .iter()
                    .position(|part| matches!(*part, "*" | "@"))
                    .ok_or_else(|| anyhow::anyhow!("mock var-create is missing a frame marker"))?;
                let name = parts
                    .get(marker.wrapping_sub(1))
                    .copied()
                    .unwrap_or("ddb_api_variable");
                let expression = parts.get(marker + 1).copied().unwrap_or("");
                let numchild = if expression.trim_matches('"') == "request" {
                    "3"
                } else {
                    "0"
                };
                let payload: Dict = vec![
                    ("name".to_string(), name.into()),
                    ("numchild".to_string(), numchild.into()),
                    ("value".to_string(), "{...}".into()),
                    ("type".to_string(), "Request".into()),
                    ("has_more".to_string(), "0".into()),
                ]
                .into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-var-list-children" => {
                let parts = args.split_whitespace().collect::<Vec<_>>();
                let object_name = parts
                    .iter()
                    .find(|part| !part.starts_with('-'))
                    .copied()
                    .unwrap_or("ddb_api_variable")
                    .trim_matches('"');
                let numbers = parts
                    .iter()
                    .rev()
                    .take(2)
                    .filter_map(|part| part.parse::<usize>().ok())
                    .collect::<Vec<_>>();
                let to = numbers.first().copied().unwrap_or(usize::MAX);
                let from = numbers.get(1).copied().unwrap_or(0);
                let definitions = if object_name.ends_with(".0") {
                    vec![
                        ("trace_id", "abc123", "const char *", 0_u64),
                        ("span_id", "def456", "const char *", 0_u64),
                    ]
                } else if object_name.contains('.') {
                    Vec::new()
                } else {
                    vec![
                        ("headers", "{...}", "HeaderMap", 2_u64),
                        ("payload", "0x1000", "void *", 0_u64),
                        ("flags", "3", "uint32_t", 0_u64),
                    ]
                };
                let total = definitions.len();
                let end = to.min(total);
                let children = definitions
                    .into_iter()
                    .enumerate()
                    .skip(from.min(total))
                    .take(end.saturating_sub(from.min(total)))
                    .map(|(index, (name, value, type_name, numchild))| {
                        Value::Dict(
                            vec![
                                ("name".to_string(), format!("{object_name}.{index}").into()),
                                ("exp".to_string(), name.into()),
                                ("value".to_string(), value.into()),
                                ("type".to_string(), type_name.into()),
                                ("numchild".to_string(), numchild.to_string().into()),
                            ]
                            .into(),
                        )
                    })
                    .collect();
                let payload: Dict = vec![
                    ("numchild".to_string(), total.to_string().into()),
                    ("children".to_string(), Value::List(children)),
                    (
                        "has_more".to_string(),
                        if end < total { "1" } else { "0" }.into(),
                    ),
                ]
                .into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-var-delete" => {
                Self::send_result(&out_tx, token, "done", None).await?;
            }
            "-data-evaluate-expression" => {
                let payload: Dict = vec![("value".to_string(), "42".into())].into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-data-read-memory-bytes" => {
                let count = args
                    .split_whitespace()
                    .last()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(8);
                let mut contents = String::with_capacity(count.saturating_mul(2));
                if count > 0 {
                    contents.push_str("2a");
                    contents.push_str(&"00".repeat(count - 1));
                }
                let payload: Dict = vec![(
                    "memory".to_string(),
                    Value::List(vec![Value::Dict(
                        vec![
                            ("begin".to_string(), "0x1000".into()),
                            ("offset".to_string(), "0".into()),
                            ("end".to_string(), format!("0x{:x}", 0x1000 + count).into()),
                            ("contents".to_string(), contents.into()),
                        ]
                        .into(),
                    )]),
                )]
                .into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-get-remote-bt" | "-serviceweaver-bt-remote" => {
                let payload = {
                    let state = state.lock().await;
                    state.dbt_payload()
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
                let payload: Dict = vec![(
                    "new-thread-id".to_string(),
                    new_thread_id.to_string().into(),
                )]
                .into();
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-switch-context-custom" => {
                let payload = {
                    let mut state = state.lock().await;
                    state.switch_context(&args)
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
            }
            "-break-insert" => {
                let enabled = !args
                    .split_whitespace()
                    .any(|argument| matches!(argument, "-d" | "--disabled"));
                let location = args
                    .split_whitespace()
                    .last()
                    .unwrap_or("main.rs:1")
                    .trim_matches(['"', '\''])
                    .to_string();
                let bkpt = {
                    let mut state = state.lock().await;
                    state.next_breakpoint(location, enabled)
                };
                Self::send_result(
                    &out_tx,
                    token,
                    "done",
                    Some(Self::breakpoint_payload(&bkpt)),
                )
                .await?;
            }
            "-break-list" => {
                let payload = {
                    let state = state.lock().await;
                    Self::breakpoint_table_payload(&state)
                };
                Self::send_result(&out_tx, token, "done", Some(payload)).await?;
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
            "-break-enable" | "-break-disable" => {
                let enabled = prefix == "-break-enable";
                let ids = args
                    .split_whitespace()
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>();
                let succeeded = if let Ok(ids) = ids {
                    let mut state = state.lock().await;
                    if ids.is_empty() || ids.iter().any(|id| !state.breakpoints.contains_key(id)) {
                        false
                    } else {
                        for id in ids {
                            state.breakpoints.get_mut(&id).unwrap().enabled = enabled;
                        }
                        true
                    }
                } else {
                    false
                };
                Self::send_result(
                    &out_tx,
                    token,
                    if succeeded { "done" } else { "error" },
                    None,
                )
                .await?;
            }
            "-break-condition" => {
                let (id, condition) = args
                    .split_once(char::is_whitespace)
                    .map(|(id, condition)| (id, Some(condition.trim())))
                    .unwrap_or((args.trim(), None));
                let condition = condition.map(|condition| {
                    serde_json::from_str::<String>(condition)
                        .unwrap_or_else(|_| condition.to_string())
                });
                let succeeded = if let Ok(id) = id.parse::<u64>() {
                    let mut state = state.lock().await;
                    if let Some(breakpoint) = state.breakpoints.get_mut(&id) {
                        breakpoint.condition = condition;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                Self::send_result(
                    &out_tx,
                    token,
                    if succeeded { "done" } else { "error" },
                    None,
                )
                .await?;
            }
            "-mock-stream-output" => {
                let (count, message) = Self::mock_output_stream(&args);
                for _ in 0..count {
                    Self::send_stream(&out_tx, "~", &message).await?;
                }
                Self::send_result(&out_tx, token, "done", None).await?;
            }
            "-mock-start-output-stream" => {
                let (count, message) = Self::mock_output_stream(&args);
                Self::send_result(&out_tx, token, "done", None).await?;
                tokio::spawn(async move {
                    for index in 0..count {
                        if Self::send_stream(&out_tx, "~", &message).await.is_err() {
                            break;
                        }
                        if (index + 1) % 16 == 0 {
                            sleep(Duration::from_millis(1)).await;
                        } else {
                            tokio::task::yield_now().await;
                        }
                    }
                });
            }
            "-exec-continue" | "-record-time-and-continue" => {
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
            "-exec-next"
            | "-exec-step"
            | "-exec-finish"
            | "-record-time-and-next"
            | "-record-time-and-step"
            | "-record-time-and-finish" => {
                {
                    let mut state = state.lock().await;
                    state.running = true;
                }
                Self::send_result(&out_tx, token, "running", None).await?;
                let running_payload: Dict = vec![(
                    "thread-id".to_string(),
                    args.split_whitespace().last().unwrap_or("all").into(),
                )]
                .into();
                Self::send_notify(&out_tx, token, "running", running_payload).await?;
                tokio::spawn(Self::schedule_step_stop(state, out_tx, token));
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
            "detach" | "kill" => {
                Self::send_result(&out_tx, token, "done", None).await?;
            }
            "exit" => {
                Self::send_result(&out_tx, token, "done", None).await?;
                out_tx.send_async(TransportEvent::Exited(Some(0))).await?;
            }
            _ => {
                Self::send_result(&out_tx, token, "done", None).await?;
            }
        }
        Ok(())
    }

    async fn run(
        state: Arc<Mutex<MockDebuggerState>>,
        requests: flume::Receiver<TransportRequest>,
        out_tx: flume::Sender<TransportEvent>,
        open: Arc<AtomicBool>,
    ) {
        while let Ok(TransportRequest::Write { data, written }) = requests.recv_async().await {
            let result = async {
                let commands = std::str::from_utf8(data.as_ref())?;
                for raw_cmd in commands.lines() {
                    let (token, prefix, args) = Self::parse_command(raw_cmd);
                    if prefix.is_empty() && args.is_empty() {
                        continue;
                    }
                    Self::handle_command(Arc::clone(&state), out_tx.clone(), token, prefix, args)
                        .await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let failure = result.as_ref().err().map(ToString::to_string);
            let _ = written.send(result);
            if let Some(error) = failure {
                let _ = out_tx
                    .send_async(TransportEvent::Fault(format!(
                        "mock debugger command failed: {}",
                        error
                    )))
                    .await;
                break;
            }
        }
        open.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl DebuggerTransport for MockAttachController {
    async fn launch(&mut self, _cmd: &str) -> Result<RunningTransport> {
        let variables_per_frame = self.state.lock().await.config.variables_per_frame;
        if variables_per_frame > MAX_MOCK_VARIABLES_PER_FRAME {
            bail!("mock variables_per_frame cannot exceed {MAX_MOCK_VARIABLES_PER_FRAME}");
        }
        let (in_tx, in_rx) = flume::bounded::<TransportRequest>(1024);
        let (out_tx, out_rx) = flume::bounded::<TransportEvent>(1024);
        self.open.store(true, Ordering::SeqCst);
        self.task = Some(tokio::spawn(Self::run(
            Arc::clone(&self.state),
            in_rx,
            out_tx,
            Arc::clone(&self.open),
        )));
        Ok(RunningTransport::new(in_tx, out_rx))
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, net::Ipv4Addr};

    use gdbmi::parser::Message;

    use super::*;
    use crate::{
        common::mock_fixture::{MockDbtParentConfig, MockStackFrameConfig},
        debugger::gdb::parser::GdbParser,
    };

    #[test]
    fn mock_variable_workload_is_deterministic_and_bounded() {
        let state = MockDebuggerState::new(
            MockSessionConfig {
                variables_per_frame: 4,
                ..MockSessionConfig::default()
            },
            9,
        );
        let payload = state.stack_variables_payload();
        let variables = payload["variables"].expect_list_ref().unwrap();
        assert_eq!(variables.len(), 4);
        assert_eq!(
            variables[2].expect_dict_ref().unwrap()["name"]
                .expect_string_ref()
                .unwrap(),
            "bench_variable_2"
        );
    }

    #[tokio::test]
    async fn mock_variable_workload_rejects_unbounded_configuration() {
        let mut controller = MockAttachController::new(
            MockSessionConfig {
                variables_per_frame: MAX_MOCK_VARIABLES_PER_FRAME + 1,
                ..MockSessionConfig::default()
            },
            9,
        );

        assert!(controller.launch("").await.is_err());
        assert!(!controller.is_open());
    }

    #[test]
    fn disabled_breakpoints_are_not_selected_as_continue_hits() {
        let mut state = MockDebuggerState::new(MockSessionConfig::default(), 9);
        state.next_breakpoint("src/main.rs:10".to_string(), false);

        assert_eq!(state.current_breakpoint_id(), None);

        let enabled = state.next_breakpoint("src/main.rs:20".to_string(), true);
        assert_eq!(state.current_breakpoint_id(), Some(enabled.id));
    }

    #[tokio::test]
    async fn mock_breakpoint_conditions_round_trip_quoted_expressions() {
        let mut controller = MockAttachController::new(MockSessionConfig::default(), 9);
        let transport = controller
            .launch("")
            .await
            .expect("mock controller should start");
        let (writer, events) = transport.into_parts();

        writer
            .write(Bytes::from_static(
                b"-break-insert src/main.rs:10\n-break-condition 1 \"request.id == \\\"special\\\"\"\n-break-list\n",
            ))
            .await
            .expect("mock breakpoint commands should send");
        let _inserted = result_payload(events.recv_async().await.unwrap());
        assert!(stdout_text(events.recv_async().await.unwrap()).contains("^done"));
        let listed = result_payload(events.recv_async().await.unwrap());
        let table = listed["BreakpointTable"].expect_dict_ref().unwrap();
        let body = table["body"].expect_list_ref().unwrap();
        let breakpoint = body[0].expect_dict_ref().unwrap()["bkpt"]
            .expect_dict_ref()
            .unwrap();

        assert_eq!(
            breakpoint["cond"].expect_string_ref().unwrap(),
            "request.id == \"special\""
        );
    }

    fn result_payload(event: TransportEvent) -> Dict {
        let TransportEvent::Stdout(line) = event else {
            panic!("expected mock protocol output");
        };
        let text = std::str::from_utf8(line.as_ref()).expect("mock output should be utf-8");
        let message = GdbParser::parse(text).expect("mock output should parse");
        match message {
            Message::Response(gdbmi::parser::Response::Result { payload, .. }) => {
                crate::debugger::gdb::parser::normalize_dict(
                    payload.expect("result response should include payload"),
                )
            }
            other => panic!("expected result response, got {other:?}"),
        }
    }

    fn stdout_text(event: TransportEvent) -> String {
        let TransportEvent::Stdout(line) = event else {
            panic!("expected mock protocol output");
        };
        std::str::from_utf8(line.as_ref())
            .expect("mock output should be utf-8")
            .to_string()
    }

    #[tokio::test]
    async fn mock_bulk_output_honors_bounded_count_and_message_bytes() {
        let mut controller = MockAttachController::new(MockSessionConfig::default(), 9);
        let transport = controller
            .launch("")
            .await
            .expect("mock controller should start");
        let (writer, events) = transport.into_parts();

        writer
            .write(Bytes::from_static(b"-mock-stream-output 3 8\n"))
            .await
            .expect("bulk output command should send");
        for _ in 0..3 {
            assert_eq!(
                stdout_text(events.recv_async().await.unwrap()),
                "~\"xxxxxxx\\n\"\n"
            );
        }
        assert!(stdout_text(events.recv_async().await.unwrap()).contains("^done"));
    }

    #[tokio::test]
    async fn mock_async_output_does_not_occupy_the_command_loop() {
        let mut controller = MockAttachController::new(MockSessionConfig::default(), 9);
        let transport = controller
            .launch("")
            .await
            .expect("mock controller should start");
        let (writer, events) = transport.into_parts();

        writer
            .write(Bytes::from_static(b"-mock-start-output-stream 32 8\n"))
            .await
            .expect("async output command should send");
        assert!(stdout_text(events.recv_async().await.unwrap()).contains("^done"));

        writer
            .write(Bytes::from_static(b"-exec-next --thread 1\n"))
            .await
            .expect("step command should not wait for async output");
        let mut output_before_step = 0;
        loop {
            let line = stdout_text(events.recv_async().await.unwrap());
            if line.contains("^running") {
                break;
            }
            if line.starts_with('~') {
                output_before_step += 1;
            }
        }
        assert!(output_before_step < 32);
    }

    #[tokio::test]
    async fn mock_stepping_actions_advance_the_top_frame_and_emit_stops() {
        let config = MockSessionConfig {
            source_file: "src/main.rs".to_string(),
            source_line: 41,
            ..MockSessionConfig::default()
        };
        let mut controller = MockAttachController::new(config, 9);
        let transport = controller
            .launch("")
            .await
            .expect("mock controller should start");
        let (writer, events) = transport.into_parts();

        for (command, expected_line) in [
            ("-exec-next", 42),
            ("-exec-step", 43),
            ("-exec-finish", 44),
            ("-record-time-and-next", 45),
            ("-record-time-and-step", 46),
            ("-record-time-and-finish", 47),
        ] {
            writer
                .write(Bytes::from(format!("{command} --thread 1\n")))
                .await
                .expect("mock stepping command should send");

            assert!(stdout_text(events.recv_async().await.unwrap()).contains("^running"));
            assert!(stdout_text(events.recv_async().await.unwrap()).contains("*running"));
            let stopped = stdout_text(events.recv_async().await.unwrap());
            assert!(stopped.contains("*stopped"), "{stopped}");
            assert!(
                stopped.contains(&format!("line=\"{expected_line}\"")),
                "{stopped}"
            );
        }

        writer
            .write(Bytes::from("-stack-list-frames\n"))
            .await
            .expect("mock stack query should send");
        let stack = result_payload(events.recv_async().await.unwrap());
        let frames = stack["stack"].expect_list_ref().unwrap();
        assert_eq!(
            frames[0].expect_dict_ref().unwrap()["line"]
                .expect_string_ref()
                .unwrap(),
            "47"
        );
    }

    #[tokio::test]
    async fn mock_dbt_commands_emit_configured_payloads() {
        let config = MockSessionConfig {
            stack_frames: vec![
                MockStackFrameConfig {
                    function: "leaf_frame".to_string(),
                    file: "leaf.rs".to_string(),
                    line: 10,
                },
                MockStackFrameConfig {
                    function: "leaf_caller".to_string(),
                    file: "leaf.rs".to_string(),
                    line: 20,
                },
            ],
            dbt_parent: Some(MockDbtParentConfig {
                ip: Ipv4Addr::new(127, 0, 0, 2),
                pid: 4242,
                tid: 1,
                proclet_id: String::new(),
                caller_ctx: BTreeMap::from([
                    ("pc".to_string(), 0x501000),
                    ("sp".to_string(), 0x7fff_2000),
                    ("fp".to_string(), 0x7fff_3000),
                ]),
            }),
            ..MockSessionConfig::default()
        };

        let mut controller = MockAttachController::new(config, 7);
        let transport = controller
            .launch("")
            .await
            .expect("mock controller should start");

        let (writer, events) = transport.into_parts();
        writer
            .write(Bytes::from(
                "-stack-list-frames\n-get-remote-bt\n-switch-context-custom pc=123 sp=456 fp=789\n",
            ))
            .await
            .expect("mock commands should send");

        let stack = result_payload(
            events
                .recv_async()
                .await
                .expect("stack-list-frames response should arrive"),
        );
        let frames = stack["stack"]
            .expect_list_ref()
            .expect("stack payload should contain frames");
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0].expect_dict_ref().unwrap()["func"]
                .expect_string_ref()
                .unwrap(),
            "leaf_frame"
        );

        let dbt = result_payload(
            events
                .recv_async()
                .await
                .expect("remote-bt response should arrive"),
        );
        assert_eq!(dbt["message"].expect_string_ref().unwrap(), "success");
        let caller_meta = dbt["metadata"].expect_dict_ref().unwrap()["caller_meta"]
            .expect_dict_ref()
            .unwrap();
        assert_eq!(caller_meta["pid"].expect_string_ref().unwrap(), "4242");
        assert_eq!(
            caller_meta["ip"].expect_string_ref().unwrap(),
            &u32::from(Ipv4Addr::new(127, 0, 0, 2)).to_string()
        );

        let switch = result_payload(
            events
                .recv_async()
                .await
                .expect("switch-context response should arrive"),
        );
        let old_ctx = switch["old_ctx"]
            .expect_dict_ref()
            .expect("switch-context should include old_ctx");
        assert_eq!(old_ctx["pc"].expect_string_ref().unwrap(), "4198400");
        assert_eq!(old_ctx["sp"].expect_string_ref().unwrap(), "2147418112");
        assert_eq!(old_ctx["fp"].expect_string_ref().unwrap(), "2147422208");
    }

    #[tokio::test]
    async fn mock_remote_bt_without_parent_returns_failed_message() {
        let mut controller = MockAttachController::new(MockSessionConfig::default(), 8);
        let transport = controller
            .launch("")
            .await
            .expect("mock controller should start");

        let (writer, events) = transport.into_parts();
        writer
            .write(Bytes::from("-get-remote-bt\n"))
            .await
            .expect("mock command should send");

        let payload = result_payload(
            events
                .recv_async()
                .await
                .expect("remote-bt response should arrive"),
        );
        assert_eq!(payload["message"].expect_string_ref().unwrap(), "failed");
    }
}
