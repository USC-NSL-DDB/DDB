//! Wire encoding of session commands.

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CompletionConsistency {
    ProtocolComplete,
    StateConsistent,
}

impl Default for CompletionConsistency {
    fn default() -> Self {
        Self::StateConsistent
    }
}

/// A command bound for one session's wire. The correlation token is not part
/// of the command: the session runtime mints it at submission, the single
/// point where commands become wire traffic.
#[derive(Debug, Clone)]
pub struct SessionCommand {
    pub command: String,
    pub thread_id: Option<u64>,
    pub consistency: CompletionConsistency,
}
