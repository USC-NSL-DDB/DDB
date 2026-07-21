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

#[derive(Debug, Clone)]
pub struct SessionCommand {
    pub token: u64,
    pub command: String,
    pub thread_id: Option<u64>,
    pub consistency: CompletionConsistency,
}

impl SessionCommand {
    pub(super) fn wire_command(&self) -> String {
        let tracked = if self.command.ends_with('\n') {
            format!("{}{}", self.token, self.command)
        } else {
            format!("{}{}\n", self.token, self.command)
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
            token: 7,
            command: "-thread-info".to_string(),
            thread_id: None,
            consistency: CompletionConsistency::StateConsistent,
        };
        assert_eq!(command.wire_command(), "7-thread-info\n");

        let already_terminated = SessionCommand {
            command: "-thread-info\n".to_string(),
            ..command.clone()
        };
        assert_eq!(already_terminated.wire_command(), "7-thread-info\n");
    }

    #[test]
    fn thread_targeting_prepends_a_thread_select() {
        let command = SessionCommand {
            token: 9,
            command: "-stack-list-frames".to_string(),
            thread_id: Some(4),
            consistency: CompletionConsistency::StateConsistent,
        };
        assert_eq!(
            command.wire_command(),
            "-thread-select 4\n9-stack-list-frames\n"
        );
    }
}
