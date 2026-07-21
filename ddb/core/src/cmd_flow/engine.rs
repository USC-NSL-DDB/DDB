use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;
use tracing::{debug, error};

use super::{
    diagnostics::DiagnosticConsole, dispatcher::CommandDispatcher, input::ParsedInputCmd,
    router::Target, CommandOutcome,
};
use crate::state::StateMgr;

const DETACHED_COMMAND_LIMIT: usize = 256;

/// Failure returned at the user-command boundary, including the MI token when
/// parsing progressed far enough to preserve it.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct CommandError {
    external_token: Option<u64>,
    #[source]
    source: anyhow::Error,
}

impl CommandError {
    fn new(external_token: Option<u64>, source: anyhow::Error) -> Self {
        Self {
            external_token,
            source,
        }
    }

    pub fn external_token(&self) -> Option<u64> {
        self.external_token
    }
}

/// Single control-plane entry point for user-originated debugger commands.
///
/// The engine owns parsing policy, target defaults, operation selection, and
/// detached-work admission. Session runtimes continue to own transport I/O,
/// response correlation, and per-session ordering.
pub struct CommandEngine {
    dispatcher: CommandDispatcher,
    diagnostics: DiagnosticConsole,
    state: Arc<StateMgr>,
    detached_slots: Arc<Semaphore>,
}

impl CommandEngine {
    pub(crate) fn new(
        dispatcher: CommandDispatcher,
        diagnostics: DiagnosticConsole,
        state: Arc<StateMgr>,
    ) -> Arc<Self> {
        Arc::new(Self {
            dispatcher,
            diagnostics,
            state,
            detached_slots: Arc::new(Semaphore::new(DETACHED_COMMAND_LIMIT)),
        })
    }

    /// Execute one CLI command to semantic completion. CLI commands without an
    /// explicit target follow the currently selected thread, then broadcast.
    pub async fn execute_cli(&self, raw: &str) -> Result<CommandOutcome, CommandError> {
        if let Some(internal) = raw.trim().strip_prefix(':') {
            return self
                .diagnostics
                .execute(internal)
                .await
                .map_err(|source| CommandError::new(None, source));
        }

        let default_target = self
            .state
            .current_thread_id()
            .map(Target::Thread)
            .unwrap_or(Target::Broadcast);
        let parsed = Self::prepare(raw, None, default_target)
            .map_err(|source| CommandError::new(None, source))?;
        self.dispatch(parsed).await
    }

    /// Execute one API command to semantic completion. API commands without an
    /// embedded or explicit target broadcast, matching the documented API policy.
    pub async fn execute_api(
        &self,
        raw: &str,
        target: Option<Target>,
    ) -> Result<CommandOutcome, CommandError> {
        let parsed = Self::prepare(raw, target, Target::Broadcast)
            .map_err(|source| CommandError::new(None, source))?;
        self.dispatch(parsed).await
    }

    /// Admit bounded detached API work. Parsing and capacity admission happen
    /// before success is returned to the caller; execution uses the same path as
    /// a waiting API request.
    pub async fn submit_api(
        self: &Arc<Self>,
        raw: &str,
        target: Option<Target>,
    ) -> Result<(), CommandError> {
        let parsed = Self::prepare(raw, target, Target::Broadcast)
            .map_err(|source| CommandError::new(None, source))?;
        let external_token = parsed.external_token;
        let permit = Arc::clone(&self.detached_slots)
            .acquire_owned()
            .await
            .map_err(|error| CommandError::new(external_token, error.into()))?;
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = engine.dispatch(parsed).await {
                error!(?error, "detached command failed");
            }
        });
        Ok(())
    }

    async fn dispatch(&self, parsed: ParsedInputCmd) -> Result<CommandOutcome, CommandError> {
        let external_token = parsed.external_token;
        let prefix = parsed.prefix.clone();
        debug!(%prefix, target = ?parsed.target, "dispatching command");
        self.dispatcher
            .dispatch(parsed)
            .await
            .map_err(|source| CommandError::new(external_token, source))
    }

    fn prepare(
        raw: &str,
        explicit_target: Option<Target>,
        default_target: Target,
    ) -> Result<ParsedInputCmd> {
        let mut parsed: ParsedInputCmd = raw.try_into()?;
        if let Some(target) = explicit_target {
            parsed.target = target;
        } else if matches!(parsed.target, Target::Unspecified) {
            parsed.target = default_target;
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_target_wins_over_ingress_default() {
        let parsed =
            CommandEngine::prepare("17-thread-info --session 4", None, Target::Broadcast).unwrap();
        assert_eq!(parsed.external_token, Some(17));
        assert_eq!(parsed.target, Target::Session(4));
    }

    #[test]
    fn explicit_api_target_wins_over_embedded_target() {
        let parsed = CommandEngine::prepare(
            "-thread-info --session 4",
            Some(Target::Session(9)),
            Target::Broadcast,
        )
        .unwrap();
        assert_eq!(parsed.target, Target::Session(9));
    }

    #[test]
    fn ingress_default_only_resolves_unspecified_target() {
        let parsed = CommandEngine::prepare("-thread-info", None, Target::Session(6)).unwrap();
        assert_eq!(parsed.target, Target::Session(6));
    }
}
