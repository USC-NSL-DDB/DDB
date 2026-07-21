//! In-flight command tracking and completion barriers.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use tokio::sync::{oneshot, watch};

use crate::cmd_flow::response::ParsedSessionResponse;

use super::{handle::CommandPermit, CompletionConsistency};

pub(super) struct PendingCommand {
    pub completion: oneshot::Sender<Result<ParsedSessionResponse>>,
    pub permit: CommandPermit,
    pub consistency: CompletionConsistency,
    pub created_at: Instant,
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
            let _ = command.completion.send(Err(error));
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(super) fn fail_all(&mut self, reason: &str) {
        for (token, command) in self.commands.drain() {
            let _ = command
                .completion
                .send(Err(anyhow!("command {} failed: {}", token, reason)));
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(super) fn sweep_expired(&mut self, timeout: Duration) {
        let expired = self
            .commands
            .iter()
            .filter_map(|(token, command)| {
                (command.created_at.elapsed() >= timeout).then_some(*token)
            })
            .collect::<Vec<_>>();
        for token in expired {
            self.fail(token, anyhow!("command {} timed out", token));
        }
    }

    pub(super) fn in_flight(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.in_flight)
    }
}

/// Completes a command once the event projector has caught up to the events
/// observed before its result, honoring the command's consistency mode.
pub(super) fn complete_after_events(
    sid: u64,
    token: u64,
    response: ParsedSessionResponse,
    command: PendingCommand,
    required_sequence: u64,
    mut applied: watch::Receiver<u64>,
    in_flight: Arc<AtomicUsize>,
) {
    tokio::spawn(async move {
        let _permit = command.permit;
        let result = if command.consistency == CompletionConsistency::StateConsistent {
            while *applied.borrow_and_update() < required_sequence {
                if applied.changed().await.is_err() {
                    break;
                }
            }
            if *applied.borrow() < required_sequence {
                Err(anyhow!(
                    "session {} event projector stopped before command {} became state-consistent",
                    sid,
                    token
                ))
            } else {
                Ok(response)
            }
        } else {
            Ok(response)
        };
        let _ = command.completion.send(result);
        in_flight.fetch_sub(1, Ordering::AcqRel);
    });
}
