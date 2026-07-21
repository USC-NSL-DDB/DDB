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

impl SessionCommand {
    pub(super) fn wire_command(&self, token: u64) -> String {
        let tracked = if self.command.ends_with('\n') {
            format!("{}{}", token, self.command)
        } else {
            format!("{}{}\n", token, self.command)
        };
        match self.thread_id {
            Some(thread_id) => format!("-thread-select {}\n{}", thread_id, tracked),
            None => tracked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_commands_are_token_prefixed_and_newline_terminated() {
        let command = SessionCommand {
            command: "-thread-info".to_string(),
            thread_id: None,
            consistency: CompletionConsistency::StateConsistent,
        };
        assert_eq!(command.wire_command(7), "7-thread-info\n");

        let already_terminated = SessionCommand {
            command: "-thread-info\n".to_string(),
            ..command.clone()
        };
        assert_eq!(already_terminated.wire_command(7), "7-thread-info\n");
    }

    #[test]
    fn thread_targeting_prepends_a_thread_select() {
        let command = SessionCommand {
            command: "-stack-list-frames".to_string(),
            thread_id: Some(4),
            consistency: CompletionConsistency::StateConsistent,
        };
        assert_eq!(
            command.wire_command(9),
            "-thread-select 4\n9-stack-list-frames\n"
        );
    }
}
