use std::{collections::HashMap, sync::Mutex};
use tracing::{debug, error, info};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Component {
    CmdFlow,
    DbgMgr,
    Api,
    Notification,
}

pub struct RuntimeStatus {
    running: tokio::sync::watch::Receiver<bool>,
    trigger: tokio::sync::watch::Sender<bool>,
    monitor: Mutex<HashMap<Component, bool>>,
}

impl RuntimeStatus {
    #[inline]
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::watch::channel(false);

        let mut monitor = HashMap::new();
        for component in &[
            Component::CmdFlow,
            Component::DbgMgr,
            Component::Api,
            Component::Notification,
        ] {
            monitor.insert(*component, false);
        }

        RuntimeStatus {
            running: rx,
            trigger: tx,
            monitor: Mutex::new(monitor),
        }
    }

    #[inline]
    pub fn up(&self, component: Component) {
        let mut status = self.monitor.lock().unwrap();
        status.insert(component, true);
        debug!("[{:?}]: Component is up.", component);

        let all_up = status.values().all(|&v| v);
        if all_up {
            self.update_status(true);
        }
    }

    #[inline]
    pub async fn wait_for_up(&self) {
        let mut rx = self.running.clone();
        loop {
            if *rx.borrow() {
                info!("[Runtime]: is up.");
                break;
            }
            match rx.changed().await {
                Ok(_) => {
                    continue;
                }
                Err(e) => {
                    error!("Error: {}", e);
                }
            }
        }
    }

    #[inline]
    pub fn update_status(&self, running: bool) {
        let _ = self.trigger.send(running);
    }

    #[inline]
    pub fn is_up(&self) -> bool {
        *self.running.borrow()
    }

    #[inline]
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.running.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::*;

    #[test]
    fn runtime_status_only_turns_up_after_required_components_are_up() {
        let status = RuntimeStatus::new();

        status.up(Component::CmdFlow);
        assert!(!status.is_up());

        status.up(Component::DbgMgr);
        assert!(!status.is_up());

        status.up(Component::Api);
        assert!(!status.is_up());

        status.up(Component::Notification);
        assert!(status.is_up());
    }

    #[tokio::test]
    async fn wait_for_up_unblocks_when_all_required_components_report_ready() {
        let status = Arc::new(RuntimeStatus::new());
        let waiter = {
            let status = Arc::clone(&status);
            tokio::spawn(async move {
                status.wait_for_up().await;
            })
        };

        status.up(Component::CmdFlow);
        status.up(Component::DbgMgr);
        status.up(Component::Api);
        status.up(Component::Notification);

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("wait_for_up should complete")
            .expect("waiter task should finish cleanly");
    }
}
