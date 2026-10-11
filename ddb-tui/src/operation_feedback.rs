//! Local intent identity is independent of DDB operation identity. A request can
//! wait in the worker queue, or fail before DDB returns an admission receipt.
use std::collections::VecDeque;

use crate::model::{App, BackendRequest};
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
            Self::DeleteBreakpoint { id, target } => {
                (format!("delete breakpoint {id}"), target.clone())
            }
            Self::SetBreakpointEnabled {
                id,
                target,
                enabled,
            } => (
                format!(
                    "{} breakpoint {id}",
                    if *enabled { "enable" } else { "disable" }
                ),
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
            Self::ConfigureLogpoint { id, .. } => return Some(format!("configure logpoint {id}")),
            _ => return None,
        };
        // Capture labels at intent time. Selecting another frame or losing the
        // resource cannot relabel a pending command as targeting something else.
        Some(format!(
            "{label} · {} · [{}]",
            app.target_label(Some(&target)),
            target_identity(&target)
        ))
    }

    #[cfg(test)]
    pub fn untracked(self) -> Self {
        match self {
            Self::Tracked { request, .. } => *request,
            request => request,
        }
    }
}

fn target_identity(target: &v2::Target) -> String {
    use v2::target::Selector;
    match target.selector.as_ref() {
        Some(Selector::Thread(target)) => target.thread_id.clone(),
        Some(Selector::Session(target)) => target.session_id.clone(),
        Some(Selector::Group(target)) => target.group_id.clone(),
        Some(Selector::SessionSet(target)) => target.session_ids.join(", "),
        Some(Selector::Multiple(target)) => target
            .targets
            .iter()
            .map(target_identity)
            .collect::<Vec<_>>()
            .join(", "),
        Some(Selector::Broadcast(_)) => "broadcast".into(),
        _ => "unresolved target".into(),
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
    retired: bool,
}

impl Entry {
    fn text(&self) -> String {
        let mut text = format!("#{} {} · {}", self.ticket, self.state.label(), self.label);
        if let Some(id) = &self.operation_id {
            text.push_str(&format!(" · {id}"));
        }
        if let Some(detail) = &self.detail {
            text.push_str(&format!(" · {detail}"));
        }
        text
    }
}

#[derive(Debug, Default)]
pub struct Feedback {
    next_ticket: u64,
    entries: VecDeque<Entry>,
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
            retired: false,
        };
        let text = entry.text();
        self.entries.push_back(entry);
        Ok((self.next_ticket, text))
    }

    /// Only the exact intent can advance. Late progress cannot regress a final
    /// result or replace the newer intent displayed in the footer.
    pub fn update(&mut self, ticket: u64, update: Update) -> Option<String> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.ticket == ticket)?;
        if entry.retired || entry.state.terminal() {
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
        (text != old).then_some(text)
    }

    /// Connection loss cannot establish failure of an already-sent mutation.
    /// A same-instance final receipt may still resolve it. A replaced instance
    /// must never update an intent belonging to the previous backend.
    pub fn disconnected(&mut self, replaced: bool) -> Vec<String> {
        self.entries
            .iter_mut()
            .filter_map(|entry| {
                if replaced {
                    entry.retired = true;
                }
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
        let entry = self.entries.back()?;
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

    pub fn has_error(&self) -> bool {
        self.entries.back().is_some_and(|entry| {
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
            .contains("#2 queued · continue to session/b"));
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
            .starts_with("1 pending · #2 completed"));
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
