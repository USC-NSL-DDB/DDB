use super::message::Notification;
use super::subscriber::{SendError, Subscriber};
use axum::extract::ws::Message;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const MAX_SUBSCRIBERS: usize = 20;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

pub struct NotificationManager {
    subscribers: Arc<DashMap<Uuid, Arc<Subscriber>>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    heartbeat_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl NotificationManager {
    pub fn new() -> Self {
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);

        Self {
            subscribers: Arc::new(DashMap::new()),
            shutdown_tx,
            heartbeat_task: tokio::sync::Mutex::new(None),
        }
    }

    /// Initialize and start background tasks
    pub async fn start(&self) {
        info!("[NotificationManager]: Starting");
        let mut task = self.heartbeat_task.lock().await;
        *task = Some(self.spawn_heartbeat_task());
    }

    /// Subscribe a new WebSocket connection
    pub fn subscribe(&self, tx: mpsc::UnboundedSender<Message>) -> Result<Uuid, SubscribeError> {
        if self.subscribers.len() >= MAX_SUBSCRIBERS {
            return Err(SubscribeError::MaxSubscribersReached);
        }

        let subscriber = Arc::new(Subscriber::new(tx));
        let id = subscriber.id;

        self.subscribers.insert(id, subscriber);
        info!(
            "New subscriber connected: {} (total: {})",
            id,
            self.subscribers.len()
        );

        Ok(id)
    }

    /// Unsubscribe a connection
    pub fn unsubscribe(&self, id: Uuid) {
        if self.subscribers.remove(&id).is_some() {
            info!(
                "Subscriber disconnected: {} (remaining: {})",
                id,
                self.subscribers.len()
            );
        }
    }

    /// Broadcast notification to all subscribers
    pub async fn broadcast(&self, notification: Notification) {
        let msg = match serde_json::to_string(&notification) {
            Ok(json) => Message::Text(json),
            Err(e) => {
                error!("Failed to serialize notification: {}", e);
                return;
            }
        };

        debug!(
            "Broadcasting notification {} to {} subscribers",
            notification.notification_id,
            self.subscribers.len()
        );

        let mut failed = Vec::new();

        for entry in self.subscribers.iter() {
            let id = *entry.key();
            let subscriber = entry.value();

            if let Err(SendError::Disconnected) = subscriber.send(msg.clone()) {
                failed.push(id);
            }
        }

        // Cleanup disconnected subscribers
        for id in failed {
            self.unsubscribe(id);
        }
    }

    /// Broadcast to specific subscriber
    pub async fn broadcast_to(
        &self,
        id: Uuid,
        notification: Notification,
    ) -> Result<(), BroadcastError> {
        let subscriber = self
            .subscribers
            .get(&id)
            .ok_or(BroadcastError::SubscriberNotFound)?;

        let msg =
            serde_json::to_string(&notification).map_err(|_| BroadcastError::SerializationError)?;

        subscriber
            .send(Message::Text(msg))
            .map_err(|_| BroadcastError::SendFailed)?;

        Ok(())
    }

    /// Get current subscriber count
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Spawn heartbeat monitoring task
    fn spawn_heartbeat_task(&self) -> tokio::task::JoinHandle<()> {
        let subscribers = Arc::clone(&self.subscribers);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            let mut ticker = interval(HEARTBEAT_INTERVAL);

            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        debug!("[NotificationManager]: Heartbeat task shutting down");
                        break;
                    }
                    _ = ticker.tick() => {
                        Self::send_heartbeats(&subscribers).await;
                    }
                }
            }
        })
    }

    async fn send_heartbeats(subscribers: &Arc<DashMap<Uuid, Arc<Subscriber>>>) {
        let mut to_remove = Vec::new();

        for entry in subscribers.iter() {
            let id = *entry.key();
            let subscriber = entry.value();

            // Check if subscriber exceeded max failed heartbeats
            if !subscriber.is_healthy().await {
                warn!(
                    "Subscriber {} exceeded max heartbeat failures, disconnecting",
                    id
                );
                to_remove.push(id);
                continue;
            }

            // Send ping
            match subscriber.send(Message::Ping(vec![])) {
                Ok(_) => {
                    subscriber.heartbeat_success().await;
                    debug!("Heartbeat sent to subscriber {}", id);
                }
                Err(_) => {
                    subscriber.heartbeat_failure().await;
                    warn!("Failed to send heartbeat to subscriber {}", id);
                }
            }
        }

        // Remove unhealthy subscribers
        for id in to_remove {
            subscribers.remove(&id);
            info!("Removed unhealthy subscriber: {}", id);
        }
    }

    /// Shutdown the notification manager
    pub async fn shutdown(&self) {
        info!("[NotificationManager]: Shutting down");
        let _ = self.shutdown_tx.send(true);

        // Wait for heartbeat task to finish
        if let Some(task) = self.heartbeat_task.lock().await.take() {
            let _ = task.await;
        }

        // Close all subscriber connections
        self.subscribers.clear();
        info!("[NotificationManager]: Shutdown complete");
    }
}

#[derive(Debug)]
pub enum SubscribeError {
    MaxSubscribersReached,
}

#[derive(Debug)]
pub enum BroadcastError {
    SubscriberNotFound,
    SerializationError,
    SendFailed,
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::notification::{Notification, NotificationPayload};

    #[test]
    fn subscribe_and_unsubscribe_update_subscriber_count() {
        let manager = NotificationManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        let id = manager
            .subscribe(tx)
            .expect("subscriber should be accepted");
        assert_eq!(manager.subscriber_count(), 1);

        manager.unsubscribe(id);
        assert_eq!(manager.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn broadcast_sends_to_active_subscribers() {
        let manager = NotificationManager::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        manager
            .subscribe(tx)
            .expect("subscriber should be accepted");

        manager
            .broadcast(Notification::new(NotificationPayload::SessionListChanged))
            .await;

        let message = rx
            .recv()
            .await
            .expect("subscriber should receive a message");
        match message {
            Message::Text(text) => {
                let value: serde_json::Value =
                    serde_json::from_str(&text.to_string()).expect("message should be valid json");
                assert_eq!(value["payload"]["type"], "SessionListChanged");
            }
            other => panic!("expected text message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn broadcast_removes_disconnected_subscribers() {
        let manager = NotificationManager::new();
        let (tx, rx) = mpsc::unbounded_channel();
        manager
            .subscribe(tx)
            .expect("subscriber should be accepted");
        drop(rx);

        manager
            .broadcast(Notification::new(NotificationPayload::SessionListChanged))
            .await;

        assert_eq!(manager.subscriber_count(), 0);
    }
}
