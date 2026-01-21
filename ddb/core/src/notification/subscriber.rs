use axum::extract::ws::Message;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

pub struct Subscriber {
    pub id: Uuid,
    pub tx: mpsc::UnboundedSender<Message>,
    pub connected_at: Instant,
    pub last_heartbeat: Arc<Mutex<Instant>>,
    pub failed_heartbeats: Arc<Mutex<u32>>,
}

impl Subscriber {
    pub fn new(tx: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            id: Uuid::new_v4(),
            tx,
            connected_at: Instant::now(),
            last_heartbeat: Arc::new(Mutex::new(Instant::now())),
            failed_heartbeats: Arc::new(Mutex::new(0)),
        }
    }

    /// Send a message to this subscriber
    pub fn send(&self, msg: Message) -> Result<(), SendError> {
        self.tx.send(msg).map_err(|_| SendError::Disconnected)
    }

    /// Check if subscriber is still responsive
    pub async fn is_healthy(&self) -> bool {
        *self.failed_heartbeats.lock().await < 5
    }

    /// Record heartbeat success
    pub async fn heartbeat_success(&self) {
        *self.last_heartbeat.lock().await = Instant::now();
        *self.failed_heartbeats.lock().await = 0;
    }

    /// Record heartbeat failure
    pub async fn heartbeat_failure(&self) {
        *self.failed_heartbeats.lock().await += 1;
    }
}

#[derive(Debug)]
pub enum SendError {
    Disconnected,
}
