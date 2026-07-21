use std::{fmt, sync::Arc};

use anyhow::Result;

use super::{
    api::{self, CommandExecutor},
    backtrace::DistributedBacktraceService,
    breakpoint::BreakpointService,
    execution::ExecutionService,
    input::ParsedInputCmd,
    query::QueryService,
    CommandOutcome, Presentation,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionAction {
    Continue,
    Interrupt,
    Next,
    Step,
    Finish,
    Jump,
    SendSignal,
    ListSignals,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryAction {
    ThreadInfo,
    ThreadSelect,
    ListThreadGroups,
    FileListLines,
}

/// Closed command vocabulary owned by the command-flow layer.
///
/// Classification is pure and explicit. Unknown commands intentionally remain
/// pass-through MI commands, while known commands select one domain operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandKind {
    BreakInsert,
    BreakDelete,
    DistributedBacktrace,
    Execution(ExecutionAction),
    Query(QueryAction),
    PassThrough,
}

impl CommandKind {
    fn classify(prefix: &str) -> Self {
        match prefix {
            "-break-insert" => Self::BreakInsert,
            "-break-delete" => Self::BreakDelete,
            "-bt-remote" => Self::DistributedBacktrace,
            "-exec-continue" | "-record-time-and-continue" => {
                Self::Execution(ExecutionAction::Continue)
            }
            "-exec-interrupt" => Self::Execution(ExecutionAction::Interrupt),
            "-exec-next" | "-record-time-and-next" => Self::Execution(ExecutionAction::Next),
            "-exec-step" | "-record-time-and-step" => Self::Execution(ExecutionAction::Step),
            "-exec-finish" | "-record-time-and-finish" => Self::Execution(ExecutionAction::Finish),
            "-exec-jump" => Self::Execution(ExecutionAction::Jump),
            "-send-signal" => Self::Execution(ExecutionAction::SendSignal),
            "-list-signals" => Self::Execution(ExecutionAction::ListSignals),
            "-thread-info" => Self::Query(QueryAction::ThreadInfo),
            "-thread-select" => Self::Query(QueryAction::ThreadSelect),
            "-list-thread-groups" => Self::Query(QueryAction::ListThreadGroups),
            "-file-list-lines" => Self::Query(QueryAction::FileListLines),
            _ => Self::PassThrough,
        }
    }
}

/// Typed application boundary between parsed user commands and domain services.
pub(crate) struct CommandDispatcher {
    breakpoints: Arc<BreakpointService>,
    execution: Arc<ExecutionService>,
    backtrace: Arc<DistributedBacktraceService>,
    queries: Arc<QueryService>,
    executor: CommandExecutor,
}

impl CommandDispatcher {
    pub(crate) fn new(
        breakpoints: Arc<BreakpointService>,
        execution: Arc<ExecutionService>,
        backtrace: Arc<DistributedBacktraceService>,
        queries: Arc<QueryService>,
        executor: CommandExecutor,
    ) -> Self {
        Self {
            breakpoints,
            execution,
            backtrace,
            queries,
            executor,
        }
    }

    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    pub(crate) async fn dispatch(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        match CommandKind::classify(&cmd.prefix) {
            CommandKind::BreakInsert => self.breakpoints.insert(cmd).await,
            CommandKind::BreakDelete => self.breakpoints.delete(cmd).await,
            CommandKind::DistributedBacktrace => self.backtrace.execute(cmd).await,
            CommandKind::Execution(action) => self.execute(action, cmd).await,
            CommandKind::Query(action) => self.query(action, cmd).await,
            CommandKind::PassThrough => self.pass_through(cmd).await,
        }
    }

    async fn execute(
        &self,
        action: ExecutionAction,
        cmd: ParsedInputCmd,
    ) -> Result<CommandOutcome> {
        match action {
            ExecutionAction::Continue => self.execution.continue_command(cmd).await,
            ExecutionAction::Interrupt => self.execution.interrupt(cmd).await,
            ExecutionAction::Next => self.execution.next(cmd).await,
            ExecutionAction::Step => self.execution.step(cmd).await,
            ExecutionAction::Finish => self.execution.finish(cmd).await,
            ExecutionAction::Jump => self.execution.jump(cmd).await,
            ExecutionAction::SendSignal => self.execution.send_signal(cmd).await,
            ExecutionAction::ListSignals => self.execution.list_signals(cmd).await,
        }
    }

    async fn query(&self, action: QueryAction, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        match action {
            QueryAction::ThreadInfo => self.queries.thread_info(cmd).await,
            QueryAction::ThreadSelect => self.queries.thread_select(cmd).await,
            QueryAction::ListThreadGroups => self.queries.list_thread_groups(cmd).await,
            QueryAction::FileListLines => self.queries.file_list_lines(cmd).await,
        }
    }

    async fn pass_through(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = self.executor.execute_plan(api::parsed(cmd)?).await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }
}

impl fmt::Debug for CommandDispatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("CommandDispatcher").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_complete_intercepted_command_vocabulary() {
        let cases = [
            ("-break-insert", CommandKind::BreakInsert),
            ("-break-delete", CommandKind::BreakDelete),
            ("-bt-remote", CommandKind::DistributedBacktrace),
            (
                "-exec-continue",
                CommandKind::Execution(ExecutionAction::Continue),
            ),
            (
                "-record-time-and-continue",
                CommandKind::Execution(ExecutionAction::Continue),
            ),
            (
                "-exec-interrupt",
                CommandKind::Execution(ExecutionAction::Interrupt),
            ),
            ("-exec-next", CommandKind::Execution(ExecutionAction::Next)),
            (
                "-record-time-and-next",
                CommandKind::Execution(ExecutionAction::Next),
            ),
            ("-exec-step", CommandKind::Execution(ExecutionAction::Step)),
            (
                "-record-time-and-step",
                CommandKind::Execution(ExecutionAction::Step),
            ),
            (
                "-exec-finish",
                CommandKind::Execution(ExecutionAction::Finish),
            ),
            (
                "-record-time-and-finish",
                CommandKind::Execution(ExecutionAction::Finish),
            ),
            ("-exec-jump", CommandKind::Execution(ExecutionAction::Jump)),
            (
                "-send-signal",
                CommandKind::Execution(ExecutionAction::SendSignal),
            ),
            (
                "-list-signals",
                CommandKind::Execution(ExecutionAction::ListSignals),
            ),
            ("-thread-info", CommandKind::Query(QueryAction::ThreadInfo)),
            (
                "-thread-select",
                CommandKind::Query(QueryAction::ThreadSelect),
            ),
            (
                "-list-thread-groups",
                CommandKind::Query(QueryAction::ListThreadGroups),
            ),
            (
                "-file-list-lines",
                CommandKind::Query(QueryAction::FileListLines),
            ),
        ];

        for (prefix, expected) in cases {
            assert_eq!(CommandKind::classify(prefix), expected, "{prefix}");
        }
        assert_eq!(
            CommandKind::classify("-interpreter-exec"),
            CommandKind::PassThrough
        );
    }
}
