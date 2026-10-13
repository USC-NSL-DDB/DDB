//! Local intent identity is independent of DDB operation identity. A request can
//! wait in the worker queue, or fail before DDB returns an admission receipt.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::model::{App, BackendRequest, Control};
use ddb_api_client::v2;

const HISTORY_LIMIT: usize = 64;

impl BackendRequest {
    pub fn feedback_label(&self, app: &App) -> Option<String> {
        let (label, target) = match self {
            Self::Control(control, target) => (control.action_name()?.to_string(), target.clone()),
            Self::Jump { location, target } => (format!("jump to {location}"), target.clone()),
            Self::SendSignal { signal, target } => (format!("signal {signal}"), target.clone()),
            Self::TerminateSession(request) => (
                "terminate session".into(),
                crate::api::session_target(&request.session.session_id),
            ),
            Self::CreateBreakpoint { target, .. } => {
                ("create breakpoint".into(), target.clone().into_api_target())
            }
            Self::DeleteBreakpoint { target, .. } => ("delete breakpoint".into(), target.clone()),
            Self::SetBreakpointEnabled {
                target, enabled, ..
            } => (
                format!("{} breakpoint", if *enabled { "enable" } else { "disable" }),
                target.clone(),
            ),
            Self::Evaluate { thread_id, .. } => (
                "evaluate expression".into(),
                crate::api::thread_target(thread_id),
            ),
            Self::AssignValue { edit, .. } => (
                "assign value".into(),
                crate::api::thread_target(&edit.owner_thread),
            ),
            Self::InvokeExtensionAction {
                extension_id,
                action_id,
                target,
                ..
            } => (
                format!("extension {extension_id}/{action_id}"),
                target.clone(),
            ),
            Self::RawCommand { target, .. } => ("debugger command".into(), target.clone()),
            Self::ConfigureLogpoint { .. } => return Some("configure logpoint".into()),
            _ => return None,
        };
        // Capture labels at intent time. Selecting another frame or losing the
        // resource cannot relabel a pending command as targeting something else.
        Some(format!("{label} · {}", app.target_label(Some(&target)),))
    }

    #[cfg(test)]
    pub fn untracked(self) -> Self {
        match self {
            Self::Tracked { request, .. } => *request,
            request => request,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Update {
    Started,
    Admitted(v2::Operation),
    Finished(v2::Operation),
    LocalCompleted,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Queued,
    Sending,
    Pending,
    Completed,
    Failed,
    Cancelled,
    Unconfirmed,
}

impl State {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Sending | Self::Pending)
    }

    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "pending admission",
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "error",
            Self::Cancelled => "cancelled",
            Self::Unconfirmed => "outcome unconfirmed",
        }
    }
}

#[derive(Debug)]
struct Entry {
    ticket: u64,
    label: String,
    state: State,
    operation_id: Option<String>,
    detail: Option<String>,
    guard: IntentGuard,
    control: Option<(Control, v2::Target)>,
    settled_at: Option<Instant>,
    dismissed: bool,
}

impl Entry {
    fn text(&self) -> String {
        let mut text = format!("{} · {}", self.state.label(), self.label);
        if let Some(detail) = &self.detail {
            text.push_str(&format!(" · {detail}"));
        }
        text
    }
}

/// The UI and worker share one retirement decision for a queued intent.
/// A connection loss alone does not retire it; an observed replacement does.
#[derive(Clone, Debug, Default)]
pub struct IntentGuard(Arc<AtomicBool>);

impl IntentGuard {
    pub fn is_retired(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Default)]
pub struct Feedback {
    next_ticket: u64,
    entries: VecDeque<Entry>,
    animation_frame: usize,
    backend_guard: IntentGuard,
}

impl Feedback {
    /// Never evict an active intent. Reject overload before sending the command.
    pub fn queue(&mut self, label: String) -> Result<(u64, String), String> {
        if self.entries.len() == HISTORY_LIMIT {
            let index = self
                .entries
                .iter()
                .position(|entry| !entry.state.active())
                .ok_or("Too many pending operations; wait before retrying")?;
            self.entries.remove(index);
        }
        self.next_ticket = self
            .next_ticket
            .checked_add(1)
            .ok_or("Operation ticket exhausted")?;
        let entry = Entry {
            ticket: self.next_ticket,
            label,
            state: State::Queued,
            operation_id: None,
            detail: None,
            guard: self.backend_guard.clone(),
            control: None,
            settled_at: None,
            dismissed: false,
        };
        let text = entry.text();
        self.entries.push_back(entry);
        Ok((self.next_ticket, text))
    }

    pub fn intent_guard(&self, ticket: u64) -> IntentGuard {
        self.entries
            .iter()
            .find(|entry| entry.ticket == ticket)
            .expect("guard requested for a queued intent")
            .guard
            .clone()
    }

    pub fn queue_control(
        &mut self,
        label: String,
        control: Control,
        target: v2::Target,
    ) -> Result<(u64, String), String> {
        let result = self.queue(label)?;
        self.entries.back_mut().expect("queued entry").control = Some((control, target));
        Ok(result)
    }

    pub fn control_pending(&self, control: Control, target: &v2::Target) -> bool {
        self.entries.iter().any(|entry| {
            entry.state.active()
                && entry
                    .control
                    .as_ref()
                    .is_some_and(|(action, scope)| *action == control && scope == target)
        })
    }

    /// Only the exact intent can advance. Late progress cannot regress a final
    /// result or replace the newer intent displayed in the footer.
    pub fn update(&mut self, ticket: u64, update: Update) -> Option<String> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.ticket == ticket)?;
        if entry.guard.is_retired() || entry.state.terminal() {
            return None;
        }
        let old = entry.text();
        match update {
            Update::Started => {
                if entry.state == State::Queued {
                    entry.state = State::Sending;
                }
            }
            Update::Admitted(operation) | Update::Finished(operation) => {
                if operation.operation_id.is_empty()
                    || entry
                        .operation_id
                        .as_ref()
                        .is_some_and(|id| id != &operation.operation_id)
                {
                    return None;
                }
                entry.operation_id = Some(operation.operation_id);
                let state = match v2::OperationState::try_from(operation.state) {
                    Ok(v2::OperationState::Completed) => State::Completed,
                    Ok(v2::OperationState::Failed) => State::Failed,
                    Ok(v2::OperationState::Cancelled) => State::Cancelled,
                    _ => State::Pending,
                };
                if entry.state != State::Unconfirmed || state.terminal() {
                    entry.state = state;
                    entry.detail = operation.error.map(|error| error.message);
                    let failed = operation
                        .target_outcomes
                        .iter()
                        .filter(|outcome| !outcome.succeeded)
                        .count();
                    if failed > 0 {
                        let summary = format!(
                            "{failed}/{} targets failed",
                            operation.target_outcomes.len()
                        );
                        entry.detail = Some(match entry.detail.take() {
                            Some(error) => format!("{summary}: {error}"),
                            None => summary,
                        });
                        // A completed fanout with failed members is not all-success.
                        if entry.state == State::Completed {
                            entry.state = State::Failed;
                        }
                    }
                }
            }
            Update::LocalCompleted => entry.state = State::Completed,
            Update::Error(error) => {
                entry.state = if entry.operation_id.is_some() {
                    State::Unconfirmed
                } else {
                    State::Failed
                };
                entry.detail = Some(error);
            }
        }
        let text = entry.text();
        if entry.state.terminal() {
            entry.settled_at = Some(Instant::now());
        }
        (text != old).then_some(text)
    }

    /// Connection loss cannot establish failure of an already-sent mutation.
    /// A same-instance final receipt may still resolve it. A replaced instance
    /// must never update an intent belonging to the previous backend.
    pub fn disconnected(&mut self, replaced: bool) -> Vec<String> {
        if replaced {
            // Worker requests can outlive the bounded display history. Retire
            // the generation, not just the entries still retained for display.
            self.backend_guard.0.store(true, Ordering::SeqCst);
            self.backend_guard = IntentGuard::default();
        }
        self.entries
            .iter_mut()
            .filter_map(|entry| {
                if !entry.state.active() {
                    return None;
                }
                entry.state = State::Unconfirmed;
                entry.detail = Some(
                    if replaced {
                        "DDB instance changed"
                    } else {
                        "connection lost"
                    }
                    .into(),
                );
                Some(entry.text())
            })
            .collect()
    }

    pub fn display(&self) -> Option<String> {
        let entry = self.entries.iter().rev().find(|entry| !entry.dismissed)?;
        let active = self
            .entries
            .iter()
            .filter(|entry| entry.state.active())
            .count();
        Some(if active > 1 || (active > 0 && !entry.state.active()) {
            format!("{active} pending · {}", entry.text())
        } else {
            entry.text()
        })
    }

    pub fn active(&self) -> bool {
        self.entries.iter().any(|entry| entry.state.active())
    }

    pub fn has_expiring_feedback(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.state.terminal() && !entry.dismissed)
    }

    pub fn tick(&mut self) {
        self.animation_frame = (self.animation_frame + 1) % 10;
        let now = Instant::now();
        for entry in &mut self.entries {
            let timeout = if entry.state == State::Completed {
                5
            } else {
                8
            };
            if entry.state.terminal()
                && entry.settled_at.is_some_and(|settled| {
                    now.saturating_duration_since(settled) >= Duration::from_secs(timeout)
                })
            {
                entry.dismissed = true;
            }
        }
    }

    pub fn indicator(&self) -> &'static str {
        if self.active() {
            self.spinner()
        } else if self.has_error() {
            "!"
        } else {
            "✓"
        }
    }

    pub fn spinner(&self) -> &'static str {
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][self.animation_frame]
    }

    pub fn diagnostics(&self) -> Option<String> {
        let entry = self.entries.iter().rev().find(|entry| !entry.dismissed)?;
        Some(format!(
            "#{} {}{}",
            entry.ticket,
            entry.text(),
            entry
                .operation_id
                .as_ref()
                .map(|id| format!(" · {id}"))
                .unwrap_or_default()
        ))
    }

    pub fn has_error(&self) -> bool {
        self.entries
            .iter()
            .rev()
            .find(|entry| !entry.dismissed)
            .is_some_and(|entry| {
                matches!(
                    entry.state,
                    State::Failed | State::Cancelled | State::Unconfirmed
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_guard_survives_disconnect_but_retires_with_its_backend() {
        let mut feedback = Feedback::default();
        let (ticket, _) = feedback.queue("continue".into()).unwrap();
        let guard = feedback.intent_guard(ticket);
        feedback.disconnected(false);
        assert!(!guard.is_retired());
        feedback.disconnected(true);
        assert!(guard.is_retired());
        let (ticket, _) = feedback.queue("continue on replacement".into()).unwrap();
        assert!(!feedback.intent_guard(ticket).is_retired());
        assert!(guard.is_retired());
    }

    #[test]
    fn evicting_feedback_history_does_not_release_a_queued_intent_from_retirement() {
        let mut feedback = Feedback::default();
        let (ticket, _) = feedback.queue("queued broadcast".into()).unwrap();
        let guard = feedback.intent_guard(ticket);
        feedback.disconnected(false);
        for index in 0..HISTORY_LIMIT {
            feedback.queue(format!("later intent {index}")).unwrap();
        }
        assert!(feedback.entries.iter().all(|entry| entry.ticket != ticket));
        feedback.disconnected(true);
        assert!(
            guard.is_retired(),
            "history eviction must not revive the worker's queued intent"
        );
    }

    #[test]
    fn confirmed_feedback_expires_but_pending_and_unconfirmed_outcomes_do_not() {
        let mut feedback = Feedback::default();
        let (completed, _) = feedback.queue("continue".into()).unwrap();
        feedback.update(completed, Update::LocalCompleted);
        assert!(feedback.display().unwrap().contains("completed"));
        assert!(feedback.has_expiring_feedback());
        feedback.entries.back_mut().unwrap().settled_at =
            Some(Instant::now() - Duration::from_secs(6));
        feedback.tick();
        assert!(feedback.display().is_none());
        assert!(feedback.diagnostics().is_none());
        assert!(!feedback.has_expiring_feedback());
        let (pending, _) = feedback.queue("pause".into()).unwrap();
        feedback.entries.back_mut().unwrap().settled_at =
            Some(Instant::now() - Duration::from_secs(600));
        feedback.tick();
        assert!(feedback.display().unwrap().contains("queued"));
        assert!(!feedback.has_expiring_feedback());
        feedback.update(
            pending,
            Update::Admitted(operation("op/pause", v2::OperationState::Running)),
        );
        feedback.disconnected(false);
        feedback.tick();
        assert!(feedback.display().unwrap().contains("unconfirmed"));
        assert!(!feedback.has_expiring_feedback());
        feedback.update(
            pending,
            Update::Finished(operation("op/pause", v2::OperationState::Completed)),
        );
        feedback.entries.back_mut().unwrap().settled_at =
            Some(Instant::now() - Duration::from_secs(6));
        feedback.tick();
        assert!(feedback.display().is_none());
    }

    #[test]
    fn pending_toolbar_identity_includes_both_action_and_target() {
        let mut feedback = Feedback::default();
        let target = crate::api::session_target("a");
        let (ticket, _) = feedback
            .queue_control("continue".into(), Control::Continue, target.clone())
            .unwrap();
        assert!(feedback.control_pending(Control::Continue, &target));
        assert!(!feedback.control_pending(Control::Interrupt, &target));
        assert!(!feedback.control_pending(Control::Continue, &crate::api::session_target("b")));
        feedback.update(ticket, Update::LocalCompleted);
        assert!(!feedback.control_pending(Control::Continue, &target));
    }

    fn operation(id: &str, state: v2::OperationState) -> v2::Operation {
        v2::Operation {
            operation_id: id.into(),
            state: state as i32,
            ..Default::default()
        }
    }

    #[test]
    fn identity_ordering_and_late_progress_do_not_replace_the_latest_intent() {
        let mut feedback = Feedback::default();
        let (first, _) = feedback.queue("next to thread/a".into()).unwrap();
        let (second, _) = feedback.queue("continue to session/b".into()).unwrap();
        feedback.update(first, Update::Started);
        feedback.update(
            first,
            Update::Admitted(operation("operation/a", v2::OperationState::Accepted)),
        );
        feedback.update(
            first,
            Update::Finished(operation("operation/a", v2::OperationState::Completed)),
        );
        assert!(feedback
            .display()
            .unwrap()
            .contains("queued · continue to session/b"));
        assert!(feedback.update(first, Update::Started).is_none());
        assert!(feedback
            .update(
                second,
                Update::Finished(operation("operation/b", v2::OperationState::Failed))
            )
            .is_some());
        assert!(feedback.has_error());
    }

    #[test]
    fn an_older_pending_action_is_visible_when_the_latest_intent_has_finished() {
        let mut feedback = Feedback::default();
        feedback.queue("next thread/a".into()).unwrap();
        let (ticket, _) = feedback.queue("continue thread/b".into()).unwrap();
        feedback.update(
            ticket,
            Update::Finished(operation("operation/b", v2::OperationState::Completed)),
        );
        assert!(feedback
            .display()
            .unwrap()
            .starts_with("1 pending · completed"));
    }

    #[test]
    fn admitted_timeout_is_unconfirmed_and_can_be_resolved_only_by_the_same_operation() {
        let mut feedback = Feedback::default();
        let (ticket, _) = feedback.queue("interrupt session/a".into()).unwrap();
        feedback.update(
            ticket,
            Update::Admitted(operation("operation/a", v2::OperationState::Accepted)),
        );
        feedback.update(ticket, Update::Error("wait timed out".into()));
        assert!(feedback.display().unwrap().contains("outcome unconfirmed"));
        assert!(feedback
            .update(
                ticket,
                Update::Finished(operation("operation/b", v2::OperationState::Completed))
            )
            .is_none());
        feedback.update(
            ticket,
            Update::Finished(operation("operation/a", v2::OperationState::Completed)),
        );
        assert!(feedback.display().unwrap().contains("completed"));
        assert!(!feedback.has_error());
    }

    #[test]
    fn disconnect_and_instance_replacement_never_claim_execution_failure() {
        let mut feedback = Feedback::default();
        let (ticket, _) = feedback.queue("next thread/a".into()).unwrap();
        assert_eq!(feedback.disconnected(false).len(), 1);
        assert!(feedback.has_error());
        feedback.disconnected(true);
        assert!(feedback
            .update(
                ticket,
                Update::Finished(operation("old", v2::OperationState::Completed))
            )
            .is_none());
        assert!(feedback.display().unwrap().contains("outcome unconfirmed"));
    }

    #[test]
    fn bounded_history_does_not_evict_active_intents_and_fanout_errors_are_visible() {
        let mut feedback = Feedback::default();
        for _ in 0..HISTORY_LIMIT {
            feedback.queue("continue".into()).unwrap();
        }
        assert!(feedback.queue("next".into()).is_err());
        let mut receipt = operation("operation/a", v2::OperationState::Completed);
        receipt.target_outcomes.push(v2::TargetOutcome {
            succeeded: false,
            ..Default::default()
        });
        feedback.update(1, Update::Finished(receipt));
        assert!(feedback.entries[0].text().contains("1/1 targets failed"));
        feedback.queue("next".into()).unwrap();
        assert_eq!(feedback.entries.len(), HISTORY_LIMIT);
        assert_eq!(feedback.entries.front().unwrap().ticket, 2);
    }
}
