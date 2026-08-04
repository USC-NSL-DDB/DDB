mod value;

use anyhow::Result;
use bytes::Bytes;

pub use value::{Dict, List, Value};

/// One semantic command submitted to a debugger protocol.
///
/// The command text is DDB's stable command vocabulary. A backend codec owns
/// token framing, thread selection, escaping, and translation into its native
/// communication interface.
#[derive(Debug, Clone, Copy)]
pub struct ProtocolCommand<'a> {
    pub token: u64,
    pub command: &'a str,
    pub thread_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StreamKind {
    Console,
    Log,
    Target,
    InferiorStdout,
    InferiorStderr,
    Prompt,
}

/// A backend-neutral record decoded from a debugger's native protocol.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProtocolRecord {
    Ready,
    Event {
        token: Option<u64>,
        message: String,
        payload: Dict,
    },
    Result {
        token: Option<u64>,
        message: String,
        payload: Option<Dict>,
    },
    Stream {
        kind: StreamKind,
        message: String,
    },
}

/// Per-session native debugger protocol.
///
/// Implementations may keep incremental parser state, so each debugger
/// session receives its own instance. The runtime never assumes newline
/// framing, MI syntax, or a particular command-completion model.
pub trait DebuggerProtocol: Send + std::fmt::Debug {
    fn starts_ready(&self) -> bool {
        true
    }
    fn encode_command(&self, command: ProtocolCommand<'_>) -> Result<Bytes>;
    fn push_stdout(&mut self, bytes: Bytes) -> Result<Vec<ProtocolRecord>>;
}
