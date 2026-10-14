use super::*;

/// Project backend automatic interrupts. Retain provenance for the exact
/// observed stop only, so a later independent stop is never hidden.
#[derive(Clone, Copy, Debug)]
pub(super) enum AutomaticPause {
    Pending(u64),
    Observed(u64),
}

pub(super) fn is_interrupt(reason: &v2::StopReason) -> bool {
    reason.kind == v2::StopReasonKind::Pause as i32
        || (reason.kind == v2::StopReasonKind::Signal as i32
            && matches!(reason.signal_name.as_deref(), Some("SIGINT" | "SIGSTOP")))
}

impl App {
    pub(super) fn apply_backend_stop_policy(
        &mut self,
        policy: Option<v2::StopPolicy>,
        threads: &[v2::Thread],
    ) {
        let Some(policy) = policy else { return };
        let previous = self.stop_policy.as_ref();
        self.all_stop_cause = policy
            .initiating_stop
            .as_ref()
            .and_then(|state| state.stop_reason.as_ref())
            .and_then(|reason| reason.thread_id.clone());
        let automatic = policy
            .pending_targets
            .iter()
            .chain(
                policy
                    .target_outcomes
                    .iter()
                    .filter(|outcome| outcome.succeeded)
                    .filter_map(|outcome| outcome.target.as_ref()),
            )
            .filter_map(|target| match target.selector.as_ref() {
                Some(v2::target::Selector::Session(session)) => Some(session.session_id.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.peer_pauses = automatic.clone();
        // Failed and retired attempts cannot explain a future stop. Keep an
        // already observed pause until its exact execution revision expires.
        self.automatic_pauses.retain(|id, provenance| {
            matches!(provenance, AutomaticPause::Observed(_))
                || threads
                    .iter()
                    .any(|thread| thread.thread_id == *id && automatic.contains(&thread.session_id))
        });
        // Outcomes are historical acknowledgments. Only a new attempt may arm
        // provenance; a later independent stop must not inherit an old success.
        let new_attempts = policy
            .pending_targets
            .iter()
            .chain(
                policy
                    .target_outcomes
                    .iter()
                    .filter(|outcome| outcome.succeeded)
                    .filter_map(|outcome| outcome.target.as_ref()),
            )
            .filter(|target| {
                previous.is_none_or(|previous| {
                    previous.generation != policy.generation
                        || (!previous.pending_targets.contains(target)
                            && (policy.pending_targets.contains(target)
                                || !previous.target_outcomes.iter().any(|outcome| {
                                    outcome.succeeded && outcome.target.as_ref() == Some(target)
                                })))
                })
            })
            .filter_map(|target| match target.selector.as_ref() {
                Some(v2::target::Selector::Session(session)) => Some(session.session_id.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        for thread in threads
            .iter()
            .filter(|thread| new_attempts.contains(&thread.session_id))
        {
            if let Some(state) = self.thread_execution(&thread.thread_id) {
                let pending = policy.pending_targets.iter().any(|target| matches!(target.selector.as_ref(),
                    Some(v2::target::Selector::Session(session)) if session.session_id == thread.session_id));
                let provenance = if state.running && pending {
                    Some(AutomaticPause::Pending(state.revision))
                } else if state.stop_reason.as_ref().is_some_and(is_interrupt) {
                    Some(AutomaticPause::Observed(state.revision))
                } else {
                    None
                };
                if let Some(provenance) = provenance {
                    self.automatic_pauses
                        .insert(thread.thread_id.clone(), provenance);
                }
            }
        }
        let new_failures = policy
            .target_outcomes
            .iter()
            .filter(|outcome| {
                previous.is_none_or(|previous| {
                    previous.generation != policy.generation
                        || !previous.target_outcomes.contains(outcome)
                })
            })
            .filter_map(|outcome| outcome.error.as_ref())
            .map(|error| error.message.clone())
            .collect::<Vec<_>>();
        for message in new_failures {
            let text = format!("DDB peer pause failed: {message}");
            self.error(text);
        }
        self.stop_policy = Some(policy);
    }

    /// A process-wide stop also stops peers, but only its reported thread owns
    /// the cause. This evidence supersedes an older focus even without Running.
    pub fn is_secondary_stop(&self, thread: &str) -> bool {
        self.thread_execution(thread).is_some_and(|state| {
            !state.running
                && state
                    .stop_reason
                    .as_ref()
                    .and_then(|reason| reason.thread_id.as_deref())
                    .is_some_and(|owner| owner != thread)
        })
    }

    pub fn is_automatic_pause(&self, thread: &str) -> bool {
        self.thread_execution(thread).is_some_and(|state| {
            !state.running
                && matches!(self.automatic_pauses.get(thread), Some(AutomaticPause::Observed(revision)) if *revision == state.revision)
        })
    }

    fn thread_execution(&self, id: &str) -> Option<&v2::ExecutionState> {
        self.execution_states.iter().find(|state| {
            matches!(state.target.as_ref().and_then(|target| target.selector.as_ref()), Some(target::Selector::Thread(thread)) if thread.thread_id == id)
        })
    }

    #[cfg(test)]
    pub fn note_automatic_pause(&mut self, session: &str) {
        for thread in self
            .threads
            .iter()
            .filter(|thread| thread.session_id == session && thread.state == "running")
        {
            let revision = self
                .thread_execution(&thread.id)
                .map_or(0, |state| state.revision);
            self.automatic_pauses
                .insert(thread.id.clone(), AutomaticPause::Pending(revision));
        }
    }

    pub(super) fn reconcile_automatic_pauses(&mut self) {
        let states = &self.execution_states;
        self.automatic_pauses.retain(|id, provenance| {
            let Some(state) = states.iter().find(|state| {
                matches!(state.target.as_ref().and_then(|target| target.selector.as_ref()), Some(target::Selector::Thread(thread)) if thread.thread_id == *id)
            }) else { return false };
            match *provenance {
                AutomaticPause::Pending(before) if state.running || state.revision == before => true,
                AutomaticPause::Pending(_) => {
                    if state.stop_reason.as_ref().is_some_and(is_interrupt) {
                        *provenance = AutomaticPause::Observed(state.revision);
                        true
                    } else { false }
                }
                AutomaticPause::Observed(revision) => !state.running && revision == state.revision,
            }
        });
    }

    pub fn cancel_pending_automatic_pauses(&mut self, session: Option<&str>) {
        let threads = &self.threads;
        self.automatic_pauses.retain(|id, provenance| {
            !matches!(provenance, AutomaticPause::Pending(_))
                || session.is_some_and(|session| {
                    threads
                        .iter()
                        .any(|thread| thread.id == *id && thread.session_id != session)
                })
        });
    }

    pub fn topology_stop_label(&self, thread: &ThreadItem) -> Option<String> {
        if !self.api_connected || thread.state != "stopped" {
            return None;
        }
        let state = self.thread_execution(&thread.id)?;
        if state.running || self.is_automatic_pause(&thread.id) {
            return None;
        }
        let reason = state.stop_reason.as_ref()?;
        if reason.thread_id.as_deref() != Some(&thread.id) {
            return None;
        }
        let label = match v2::StopReasonKind::try_from(reason.kind).ok()? {
            v2::StopReasonKind::Breakpoint => "breakpoint hit".to_string(),
            v2::StopReasonKind::Step => "step complete".to_string(),
            v2::StopReasonKind::Pause => "pause".to_string(),
            v2::StopReasonKind::Signal => format!(
                "signal {}",
                reason.signal_name.as_deref().unwrap_or("unknown")
            ),
            v2::StopReasonKind::Exception => "exception".to_string(),
            v2::StopReasonKind::Watchpoint => "watchpoint".to_string(),
            v2::StopReasonKind::Entry => "entry".to_string(),
            v2::StopReasonKind::Exit => "exit".to_string(),
            v2::StopReasonKind::Other => reason
                .description
                .clone()
                .unwrap_or_else(|| "other stop".into()),
            v2::StopReasonKind::Unspecified => return None,
        };
        Some(format!("trigger · {label}"))
    }

    pub(super) fn session_stop_label(&self, session: &str) -> Option<String> {
        let mut labels = self
            .threads
            .iter()
            .filter(|thread| thread.session_id == session)
            .filter_map(|thread| self.topology_stop_label(thread))
            .collect::<Vec<_>>();
        labels.sort();
        labels.dedup();
        (!labels.is_empty()).then(|| labels.join("; "))
    }
}
