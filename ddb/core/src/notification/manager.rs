use std::sync::{Arc, Weak};

use axum::extract::ws::Message;
use dashmap::DashMap;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::{message::Notification, subscriber::Subscriber};

pub(super) const MAX_SUBSCRIBERS: usize = 20;
const SUBSCRIBER_QUEUE_CAPACITY: usize = 128;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

pub(super) struct Subscription {
    id: Uuid,
    receiver: mpsc::Receiver<Message>,
    manager: Weak<NotificationManager>,
}

impl Subscription {
    pub(super) fn id(&self) -> Uuid {
        self.id
    }

    pub(super) async fn recv(&mut self) -> Option<Message> {
        self.receiver.recv().await
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(manager) = self.manager.upgrade() {
            manager.unsubscribe(self.id);
        }
    }
}

pub struct NotificationManager {
    subscribers: Arc<DashMap<Uuid, Subscriber>>,
    capacity: Arc<Semaphore>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    heartbeat_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationManager {
    pub fn new() -> Self {
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);

        Self {
            subscribers: Arc::new(DashMap::new()),
            capacity: Arc::new(Semaphore::new(MAX_SUBSCRIBERS)),
            shutdown_tx,
            heartbeat_task: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn start(&self) {
        let mut task = self.heartbeat_task.lock().await;
        if task.is_some() {
            return;
        }
        info!("[NotificationManager]: Starting");
        *task = Some(self.spawn_heartbeat_task());
    }

    pub(super) fn subscribe(self: &Arc<Self>) -> Result<Subscription, SubscribeError> {
        let capacity_permit = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| SubscribeError::MaxSubscribersReached)?;
        let (tx, receiver) = mpsc::channel(SUBSCRIBER_QUEUE_CAPACITY);
        let subscriber = Subscriber::new(tx, capacity_permit);
        let id = subscriber.id();

        self.subscribers.insert(id, subscriber);
        info!(
            "New subscriber connected: {} (total: {})",
            id,
            self.subscribers.len()
        );

        Ok(Subscription {
            id,
            receiver,
            manager: Arc::downgrade(self),
        })
    }

    fn unsubscribe(&self, id: Uuid) {
        if self.subscribers.remove(&id).is_some() {
            info!(
                "Subscriber disconnected: {} (remaining: {})",
                id,
                self.subscribers.len()
            );
        }
    }

    pub async fn broadcast(&self, notification: Notification) {
        let message = match serde_json::to_string(&notification) {
            Ok(json) => Message::Text(json),
            Err(error) => {
                error!("Failed to serialize notification: {}", error);
                return;
            }
        };

        debug!(
            "Broadcasting notification {} to {} subscribers",
            notification.notification_id,
            self.subscribers.len()
        );

        let failed = self
            .subscribers
            .iter()
            .filter_map(|entry| {
                entry
                    .value()
                    .send(message.clone())
                    .err()
                    .map(|error| (*entry.key(), error))
            })
            .collect::<Vec<_>>();

        for (id, error) in failed {
            warn!("Removing subscriber {} after send failed: {:?}", id, error);
            self.unsubscribe(id);
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    pub(super) fn record_pong(&self, id: Uuid) {
        if let Some(subscriber) = self.subscribers.get(&id) {
            subscriber.record_pong();
        }
    }

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
                        Self::send_heartbeats(&subscribers);
                    }
                }
            }
        })
    }

    fn send_heartbeats(subscribers: &DashMap<Uuid, Subscriber>) {
        let failed = subscribers
            .iter()
            .filter_map(|entry| {
                let id = *entry.key();
                let subscriber = entry.value();
                if !subscriber.is_healthy() {
                    warn!(
                        "Subscriber {} exceeded max heartbeat failures, disconnecting",
                        id
                    );
                    return Some(id);
                }

                subscriber.record_ping();
                match subscriber.send(Message::Ping(Vec::new())) {
                    Ok(()) => {
                        debug!("Heartbeat sent to subscriber {}", id);
                        None
                    }
                    Err(error) => {
                        warn!("Failed to send heartbeat to subscriber {}: {:?}", id, error);
                        Some(id)
                    }
                }
            })
            .collect::<Vec<_>>();

        for id in failed {
            subscribers.remove(&id);
            info!("Removed unhealthy subscriber: {}", id);
        }
    }

    pub async fn shutdown(&self) {
        info!("[NotificationManager]: Shutting down");
        let _ = self.shutdown_tx.send(true);

        if let Some(task) = self.heartbeat_task.lock().await.take() {
            let _ = task.await;
        }

        self.subscribers.clear();
        info!("[NotificationManager]: Shutdown complete");
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SubscribeError {
    MaxSubscribersReached,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification::{Notification, NotificationPayload};

    #[test]
    fn subscription_drop_unregisters_and_releases_capacity() {
        let manager = Arc::new(NotificationManager::new());
        let subscription = manager.subscribe().unwrap();
        assert_eq!(manager.subscriber_count(), 1);

        drop(subscription);

        assert_eq!(manager.subscriber_count(), 0);
        assert_eq!(manager.capacity.available_permits(), MAX_SUBSCRIBERS);
    }

    #[test]
    fn subscriber_limit_is_enforced_by_owned_permits() {
        let manager = Arc::new(NotificationManager::new());
        let subscriptions = (0..MAX_SUBSCRIBERS)
            .map(|_| manager.subscribe().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            manager.subscribe().err(),
            Some(SubscribeError::MaxSubscribersReached)
        );

        drop(subscriptions);
        assert!(manager.subscribe().is_ok());
    }

    #[tokio::test]
    async fn broadcast_sends_to_active_subscribers() {
        let manager = Arc::new(NotificationManager::new());
        let mut subscription = manager.subscribe().unwrap();

        manager
            .broadcast(Notification::new(NotificationPayload::SessionListChanged))
            .await;

        let message = subscription.recv().await.unwrap();
        match message {
            Message::Text(text) => {
                let value: serde_json::Value = serde_json::from_str(&text.to_string()).unwrap();
                assert_eq!(value["payload"]["type"], "SessionListChanged");
            }
            other => panic!("expected text message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn broadcast_removes_subscriber_when_its_queue_is_full() {
        let manager = Arc::new(NotificationManager::new());
        let _subscription = manager.subscribe().unwrap();

        for _ in 0..=SUBSCRIBER_QUEUE_CAPACITY {
            manager
                .broadcast(Notification::new(NotificationPayload::SessionListChanged))
                .await;
        }

        assert_eq!(manager.subscriber_count(), 0);
        assert_eq!(manager.capacity.available_permits(), MAX_SUBSCRIBERS);
    }

    #[test]
    fn heartbeat_timeout_removes_unresponsive_subscriber() {
        let manager = Arc::new(NotificationManager::new());
        let _subscription = manager.subscribe().unwrap();

        for _ in 0..=5 {
            NotificationManager::send_heartbeats(&manager.subscribers);
        }

        assert_eq!(manager.subscriber_count(), 0);
    }

    #[test]
    fn pong_resets_the_missed_heartbeat_count() {
        let manager = Arc::new(NotificationManager::new());
        let subscription = manager.subscribe().unwrap();

        for _ in 0..4 {
            NotificationManager::send_heartbeats(&manager.subscribers);
        }
        manager.record_pong(subscription.id());
        for _ in 0..4 {
            NotificationManager::send_heartbeats(&manager.subscribers);
        }

        assert_eq!(manager.subscriber_count(), 1);
    }
}
