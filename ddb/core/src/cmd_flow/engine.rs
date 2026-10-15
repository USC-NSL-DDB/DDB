use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;
use tracing::{debug, error};

use super::{
    diagnostics::DiagnosticConsole,
    dispatcher::CommandDispatcher,
    input::{CommandMetadata, ParsedInputCmd},
    router::Target,
    CommandOutcome,
};
use crate::state::RuntimeModel;

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

    pub(crate) fn fanout_report(&self) -> Option<&super::router::CommandFanoutReport> {
        self.source
            .downcast_ref::<super::router::CommandFanoutError>()
            .map(super::router::CommandFanoutError::report)
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
    model: Arc<RuntimeModel>,
    detached_slots: Arc<Semaphore>,
}

impl CommandEngine {
    pub(crate) fn new(
        dispatcher: CommandDispatcher,
        diagnostics: DiagnosticConsole,
        model: Arc<RuntimeModel>,
    ) -> Arc<Self> {
        Arc::new(Self {
            dispatcher,
            diagnostics,
            model,
            detached_slots: Arc::new(Semaphore::new(DETACHED_COMMAND_LIMIT)),
        })
    }

    /// Execute one CLI command to semantic completion. CLI commands without an
    /// explicit target follow the currently selected thread, then broadcast.
    pub async fn execute_cli(&self, raw: &str) -> Result<CommandOutcome, CommandError> {
        if let Some(internal) = raw.trim().strip_prefix(':') {
            return self
                .diagnostics
                .execute(internal, CommandMetadata::default())
                .await
                .map_err(|source| CommandError::new(None, source));
        }

        let default_target = self
            .model
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
        if let Some(internal) = raw.trim().strip_prefix(':') {
            return self
                .diagnostics
                .execute(internal, CommandMetadata::default())
                .await
                .map_err(|source| CommandError::new(None, source));
        }
        let parsed = Self::prepare(raw, target, Target::Broadcast)
            .map_err(|source| CommandError::new(None, source))?;
        self.dispatch(parsed).await
    }

    /// Execute an API command with safe application-operation correlation.
    /// Metadata remains out-of-band and is never included in debugger input.
    pub(crate) async fn execute_api_with_metadata(
        &self,
        raw: &str,
        target: Option<Target>,
        metadata: super::input::CommandMetadata,
    ) -> Result<CommandOutcome, CommandError> {
        if let Some(internal) = raw.trim().strip_prefix(':') {
            return self
                .diagnostics
                .execute(internal, metadata)
                .await
                .map_err(|source| CommandError::new(None, source));
        }
        let parsed = Self::prepare(raw, target, Target::Broadcast)
            .map_err(|source| CommandError::new(None, source))?
            .with_metadata(metadata);
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
        if raw.trim().starts_with(':') {
            let engine = Arc::clone(self);
            let command = raw.to_string();
            let permit = Arc::clone(&self.detached_slots)
                .acquire_owned()
                .await
                .map_err(|error| CommandError::new(None, error.into()))?;
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = engine.execute_api(&command, target).await {
                    error!(?error, "detached diagnostic command failed");
                }
            });
            return Ok(());
        }
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
        retire_legacy_stop_automation(&self.model, &parsed, |target| {
            self.dispatcher.resolve_session_ids(target)
        })
        .map_err(|source| CommandError::new(parsed.external_token, source))?;
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

pub(super) fn retire_legacy_stop_automation(
    model: &RuntimeModel,
    parsed: &ParsedInputCmd,
    resolve_sessions: impl FnOnce(&Target) -> Result<Vec<u64>>,
) -> Result<()> {
    // Typed application operations retire automation at acceptance. Retiring
    // again here would cancel work for a stop that arrived after acceptance,
    // including when this command's own execution guard later rejects it.
    if parsed.metadata.operation_kind.is_some() {
        return Ok(());
    }
    let automatic = parsed
        .metadata
        .dispatch_guard
        .as_ref()
        .is_some_and(|guard| guard.is_automatic());
    if !automatic
        && (parsed.prefix.starts_with("-exec-")
            || parsed.prefix.starts_with("-record-time-and-")
            || matches!(
                parsed.prefix.as_str(),
                "-send-signal"
                    | "-interpreter-exec"
                    | "-var-assign"
                    | "-data-evaluate-expression"
                    | "-break-update"
                    | "-break-delete"
                    | "-break-enable"
                    | "-break-disable"
                    | "-break-condition"
            ))
    {
        if matches!(
            parsed.prefix.as_str(),
            "-exec-interrupt" | "-exec-interrupt-if-running"
        ) {
            model.retire_stop_automation_for_pauses(&resolve_sessions(&parsed.target)?);
        } else {
            model.retire_stop_automation();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ddb_api_types::v2::OperationKind;

    use crate::{
        cmd_flow::input::{CommandMetadata, DispatchGuard},
        state::ThreadStatus,
    };

    use super::*;

    const INVALIDATING_COMMANDS: &[&str] = &[
        "-exec-continue",
        "-exec-next",
        "-exec-step",
        "-exec-finish",
        "-exec-jump",
        "-exec-interrupt",
        "-exec-interrupt-if-running",
        "-record-time-and-continue",
        "-record-time-and-next",
        "-record-time-and-step",
        "-record-time-and-finish",
        "-send-signal",
        "-interpreter-exec",
        "-var-assign",
        "-data-evaluate-expression",
        "-break-update",
        "-break-delete",
        "-break-enable",
        "-break-disable",
        "-break-condition",
    ];

    #[derive(Debug)]
    struct AutomaticGuard;

    #[async_trait::async_trait]
    impl DispatchGuard for AutomaticGuard {
        async fn check(&self) -> Result<()> {
            Ok(())
        }

        fn is_automatic(&self) -> bool {
            true
        }
    }

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

    #[tokio::test]
    async fn admitted_command_preserves_automation_for_a_later_native_stop() {
        let model = RuntimeModel::new();
        model.register_session(1, "fixture", None).await;
        model.complete_session_activation(1, None).await;
        model.register_thread_group(1, "i1").await.unwrap();
        let thread = model.register_thread(1, 1, "i1").await.unwrap().thread_id;
        model
            .update_thread_statuses(1, &[1], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let original = model.thread_snapshots_for_sessions(&[1]).await[0].stop_revision;

        // The application accepted this explicit intent and retired the work
        // for its original stop before the command task reached the engine.
        model.retire_stop_automation();
        model
            .update_thread_statuses(1, &[1], ThreadStatus::RUNNING)
            .await
            .unwrap();
        model
            .update_thread_statuses(1, &[1], ThreadStatus::STOPPED)
            .await
            .unwrap();
        let control = model.execution_control_epoch();
        let later_automation = control.current();
        let parsed = CommandEngine::prepare("-exec-continue", None, Target::Thread(thread))
            .unwrap()
            .with_metadata(CommandMetadata {
                operation_id: Some("accepted-before-later-stop".into()),
                operation_kind: Some(OperationKind::Execute as u32),
                ..Default::default()
            });

        retire_legacy_stop_automation(&model, &parsed, |_| {
            panic!("admitted intent must not resolve targets for a second retirement")
        })
        .unwrap();
        assert!(model
            .prepare_stop_for_dispatch(thread, original)
            .await
            .is_none());
        assert!(control.admit(later_automation, || ()).is_some());
    }

    #[test]
    fn legacy_commands_retire_automation_and_mark_explicit_pause_targets() {
        for command in INVALIDATING_COMMANDS {
            let model = RuntimeModel::new();
            let control = model.execution_control_epoch();
            let parsed = CommandEngine::prepare(command, None, Target::Session(9)).unwrap();
            retire_legacy_stop_automation(&model, &parsed, |target| {
                assert_eq!(target, &Target::Session(9));
                Ok(vec![9])
            })
            .unwrap();
            assert_eq!(control.current(), 1, "{command}");
            assert_eq!(
                control.has_explicit_pause(9, 1),
                matches!(*command, "-exec-interrupt" | "-exec-interrupt-if-running"),
                "{command}"
            );
        }
        let model = RuntimeModel::new();
        let parsed = CommandEngine::prepare("-stack-list-frames", None, Target::Broadcast).unwrap();
        retire_legacy_stop_automation(&model, &parsed, |_| {
            panic!("inspection must not resolve pause targets")
        })
        .unwrap();
        assert_eq!(model.execution_control_epoch().current(), 0);
    }

    #[test]
    fn typed_and_automatic_commands_preserve_current_automation() {
        for command in INVALIDATING_COMMANDS {
            for metadata in [
                CommandMetadata {
                    operation_id: Some("admitted".into()),
                    operation_kind: Some(OperationKind::Execute as u32),
                    ..Default::default()
                },
                CommandMetadata {
                    dispatch_guard: Some(Arc::new(AutomaticGuard)),
                    ..Default::default()
                },
            ] {
                let model = RuntimeModel::new();
                let parsed = CommandEngine::prepare(command, None, Target::Session(9))
                    .unwrap()
                    .with_metadata(metadata);
                retire_legacy_stop_automation(&model, &parsed, |_| {
                    panic!("typed or automatic work must not retire stop automation again")
                })
                .unwrap();
                assert_eq!(model.execution_control_epoch().current(), 0, "{command}");
            }
        }
    }
}
