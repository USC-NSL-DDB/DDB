mod decode;
mod reducer;
mod render;

pub(crate) use decode::decode_event;
pub(crate) use reducer::DebuggerEventReducer;

use crate::debugger::protocol::{Dict, StreamKind};

use crate::session::lifecycle::SessionTerminationCause;
use crate::state::ThreadLocation;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum ThreadSet {
    All,
    One(u64),
    Many(Vec<u64>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum DebuggerEventKind {
    BreakpointModified,
    BreakpointDeleted {
        local_breakpoint_id: u64,
    },
    ThreadCreated {
        local_thread_id: u64,
        local_group_id: String,
    },
    ThreadExited {
        local_thread_id: u64,
        local_group_id: String,
    },
    Running {
        threads: ThreadSet,
    },
    Stopped {
        reasons: Vec<String>,
        thread: Option<ThreadSet>,
        stopped_threads: Option<ThreadSet>,
        local_breakpoint_id: Option<u64>,
        location: Option<ThreadLocation>,
    },
    ThreadGroupAdded {
        local_group_id: String,
    },
    ThreadGroupRemoved {
        local_group_id: String,
    },
    ThreadGroupStarted {
        local_group_id: String,
        pid: u64,
    },
    ThreadGroupExited {
        local_group_id: String,
    },
    Unknown,
}

#[derive(Debug, Clone)]
pub(crate) struct DebuggerEvent {
    pub token: Option<u64>,
    pub message: String,
    pub payload: Dict,
    pub kind: DebuggerEventKind,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectedDebuggerRecord {
    pub prefix: &'static str,
    /// Stable semantic stream name for raw debugger output. Async records use
    /// `None` and derive their API stream from the MI-compatible prefix.
    pub stream: Option<&'static str>,
    pub message: String,
    pub payload: Option<Dict>,
    pub token: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectedDebuggerOutput {
    pub records: Vec<ProjectedDebuggerRecord>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EventProjection {
    pub output: Option<ProjectedDebuggerOutput>,
    pub lifecycle: Option<SessionTerminationCause>,
}

impl EventProjection {
    fn record(prefix: &'static str, message: String, payload: Dict, token: Option<u64>) -> Self {
        Self {
            output: Some(ProjectedDebuggerOutput {
                records: vec![ProjectedDebuggerRecord {
                    prefix,
                    stream: None,
                    message,
                    payload: Some(payload),
                    token,
                }],
            }),
            lifecycle: None,
        }
    }

    fn records(records: Vec<ProjectedDebuggerRecord>) -> Self {
        Self {
            output: (!records.is_empty()).then_some(ProjectedDebuggerOutput { records }),
            lifecycle: None,
        }
    }

    fn exited(reasons: Vec<String>) -> Self {
        Self {
            output: None,
            lifecycle: Some(SessionTerminationCause::ProtocolExit { reasons }),
        }
    }

    pub(crate) fn debugger_stream(kind: StreamKind, message: String) -> Self {
        let (prefix, stream, message) = match kind {
            StreamKind::Console => ("~", "console", message),
            StreamKind::Log => ("&", "log", message),
            StreamKind::Target => ("@", "target", message),
            StreamKind::InferiorStdout => ("@", "inferior_stdout", message),
            StreamKind::InferiorStderr => ("&", "inferior_stderr", message),
            StreamKind::Prompt => (
                "",
                "prompt",
                if message.is_empty() {
                    "(gdb)".to_string()
                } else {
                    message
                },
            ),
        };
        Self {
            output: Some(ProjectedDebuggerOutput {
                records: vec![ProjectedDebuggerRecord {
                    prefix,
                    stream: Some(stream),
                    message,
                    payload: None,
                    token: None,
                }],
            }),
            lifecycle: None,
        }
    }
}
