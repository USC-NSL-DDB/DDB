use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use tracing::{debug, error, info};

static RUNTIME_STATUS: OnceLock<RuntimeStatus> = OnceLock::new();

fn init_rt_status() -> &'static RuntimeStatus {
    RUNTIME_STATUS.get_or_init(|| RuntimeStatus::new())
}

pub fn get_rt_status() -> &'static RuntimeStatus {
    init_rt_status()
}

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
        for component in &[Component::CmdFlow, Component::DbgMgr, Component::Notification] {
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
