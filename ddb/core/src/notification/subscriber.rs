use std::sync::atomic::{AtomicU8, Ordering};

use axum::extract::ws::Message;
use tokio::sync::{mpsc, OwnedSemaphorePermit};
use uuid::Uuid;

const MAX_MISSED_HEARTBEATS: u8 = 5;

pub(super) struct Subscriber {
    id: Uuid,
    tx: mpsc::Sender<Message>,
    missed_heartbeats: AtomicU8,
    _capacity_permit: OwnedSemaphorePermit,
}

impl Subscriber {
    pub(super) fn new(tx: mpsc::Sender<Message>, capacity_permit: OwnedSemaphorePermit) -> Self {
        Self {
            id: Uuid::new_v4(),
            tx,
            missed_heartbeats: AtomicU8::new(0),
            _capacity_permit: capacity_permit,
        }
    }

    pub(super) fn id(&self) -> Uuid {
        self.id
    }

    pub(super) fn send(&self, message: Message) -> Result<(), SendError> {
        self.tx.try_send(message).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SendError::Overloaded,
            mpsc::error::TrySendError::Closed(_) => SendError::Disconnected,
        })
    }

    pub(super) fn is_healthy(&self) -> bool {
        self.missed_heartbeats.load(Ordering::Acquire) < MAX_MISSED_HEARTBEATS
    }

    pub(super) fn record_pong(&self) {
        self.missed_heartbeats.store(0, Ordering::Release);
    }

    pub(super) fn record_ping(&self) {
        self.missed_heartbeats
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |missed| {
                Some(missed.saturating_add(1))
            })
            .ok();
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SendError {
    Disconnected,
    Overloaded,
}
