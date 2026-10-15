//! In-flight command tracking and completion barriers.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use tokio::{
    sync::{oneshot, watch},
    time::Instant,
};

use crate::cmd_flow::response::ParsedSessionResponse;

use super::{
    handle::{CommandPermit, PendingRegistration},
    CompletionConsistency,
};

pub(super) struct PendingCommand {
    pub completion: oneshot::Sender<Result<ParsedSessionResponse>>,
    pub permit: CommandPermit,
    pub consistency: CompletionConsistency,
    pub deadline: Instant,
    pub _registration: PendingRegistration,
}

/// Commands awaiting their result, sharing the runtime's in-flight counter.
///
/// Every insertion increments the counter; the matching decrement happens on
/// the failure paths here or after a completion barrier resolves.
pub(super) struct PendingCommands {
    commands: HashMap<u64, PendingCommand>,
    in_flight: Arc<AtomicUsize>,
}

impl PendingCommands {
    pub(super) fn new(in_flight: Arc<AtomicUsize>) -> Self {
        Self {
            commands: HashMap::new(),
            in_flight,
        }
    }

    pub(super) fn contains(&self, token: u64) -> bool {
        self.commands.contains_key(&token)
    }

    pub(super) fn insert(&mut self, token: u64, command: PendingCommand) {
        self.commands.insert(token, command);
        self.in_flight.fetch_add(1, Ordering::AcqRel);
    }

    /// Removes the command for a completion barrier; the barrier owns the
    /// in-flight decrement once it resolves.
    pub(super) fn take(&mut self, token: u64) -> Option<PendingCommand> {
        self.commands.remove(&token)
    }

    pub(super) fn fail(&mut self, token: u64, error: anyhow::Error) {
        if let Some(command) = self.commands.remove(&token) {
            finish_command(command, Err(error), &self.in_flight);
        }
    }

    pub(super) fn fail_all(&mut self, reason: &str) {
        for (token, command) in self.commands.drain() {
            finish_command(
                command,
                Err(anyhow!("command {} failed: {}", token, reason)),
                &self.in_flight,
            );
        }
    }

    pub(super) fn sweep_expired(&mut self) {
        let now = Instant::now();
        let expired = self
            .commands
            .iter()
            .filter_map(|(token, command)| (command.deadline <= now).then_some(*token))
            .collect::<Vec<_>>();
        for token in expired {
            self.fail(token, anyhow!("command {} timed out", token));
        }
    }

    pub(super) fn in_flight(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.in_flight)
    }

    pub(super) fn nearest_deadline(&self) -> Option<Instant> {
        self.commands.values().map(|command| command.deadline).min()
    }
}

fn finish_command(
    command: PendingCommand,
    result: Result<ParsedSessionResponse>,
    in_flight: &AtomicUsize,
) {
    let PendingCommand {
        completion,
        permit,
        _registration,
        ..
    } = command;
    drop(permit);
    drop(_registration);
    in_flight.fetch_sub(1, Ordering::AcqRel);
    let _ = completion.send(result);
}

/// Completes a command once the event projector has caught up to the events
/// observed before its result, honoring its consistency mode and original
/// deadline. A closed completion receiver also releases the barrier's ownership.
///
/// The common case — protocol-complete commands and state-consistent commands
/// whose observed events are already projected — completes inline on the
/// caller's task. A barrier task is spawned only when completion must actually
/// wait for the projector.
pub(super) fn complete_after_events(
    sid: u64,
    token: u64,
    response: ParsedSessionResponse,
    mut command: PendingCommand,
    required_sequence: u64,
    mut applied: watch::Receiver<u64>,
    in_flight: Arc<AtomicUsize>,
) {
    if command.deadline <= Instant::now() {
        finish_command(
            command,
            Err(anyhow!("command {} timed out", token)),
            &in_flight,
        );
        return;
    }
    if command.consistency != CompletionConsistency::StateConsistent
        || *applied.borrow() >= required_sequence
    {
        finish_command(command, Ok(response), &in_flight);
        return;
    }

    tokio::spawn(async move {
        let result = tokio::select! {
            _ = command.completion.closed() => {
                Err(anyhow!("command {} completion was cancelled", token))
            }
            result = tokio::time::timeout_at(command.deadline, async {
                while *applied.borrow_and_update() < required_sequence {
                    if applied.changed().await.is_err() {
                        return Err(anyhow!(
                            "session {} event projector stopped before command {} became state-consistent",
                            sid,
                            token
                        ));
                    }
                }
                Ok(response)
            }) => result.unwrap_or_else(|_| Err(anyhow!("command {} timed out", token))),
        };
        finish_command(command, result, &in_flight);
    });
}
