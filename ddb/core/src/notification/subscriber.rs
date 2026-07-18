use axum::extract::ws::Message;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

pub struct Subscriber {
    pub id: Uuid,
    pub tx: mpsc::Sender<Message>,
    pub connected_at: Instant,
    pub last_heartbeat: Arc<Mutex<Instant>>,
    pub failed_heartbeats: Arc<Mutex<u32>>,
}

impl Subscriber {
    pub fn new(tx: mpsc::Sender<Message>) -> Self {
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
        self.tx.try_send(msg).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SendError::Overloaded,
            mpsc::error::TrySendError::Closed(_) => SendError::Disconnected,
        })
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
    Overloaded,
}

#[cfg(test)]
mod tests {
    use axum::extract::ws::Message;
    use tokio::sync::mpsc;

    use super::*;

    #[tokio::test]
    async fn heartbeat_failures_flip_health_until_success_resets_it() {
        let (tx, _rx) = mpsc::channel(1);
        let subscriber = Subscriber::new(tx);

        for _ in 0..4 {
            subscriber.heartbeat_failure().await;
        }
        assert!(subscriber.is_healthy().await);

        subscriber.heartbeat_failure().await;
        assert!(!subscriber.is_healthy().await);

        subscriber.heartbeat_success().await;
        assert!(subscriber.is_healthy().await);
    }

    #[test]
    fn send_reports_disconnected_when_receiver_is_gone() {
        let (tx, rx) = mpsc::channel(1);
        let subscriber = Subscriber::new(tx);
        drop(rx);

        assert!(matches!(
            subscriber.send(Message::Text("payload".into())),
            Err(SendError::Disconnected)
        ));
    }

    #[test]
    fn send_reports_overload_when_queue_is_full() {
        let (tx, _rx) = mpsc::channel(1);
        let subscriber = Subscriber::new(tx);

        subscriber
            .send(Message::Text("first".into()))
            .expect("first message should fit");
        assert!(matches!(
            subscriber.send(Message::Text("second".into())),
            Err(SendError::Overloaded)
        ));
    }
}
