use lazy_static::lazy_static;
use std::{
    collections::HashMap, sync::{Mutex, OnceLock}, time::Duration
};
use tokio::sync::{oneshot, watch};
use tracing::{debug, error, info};

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

lazy_static! {
    pub static ref SHUTDOWN_SIGNAL: ShutdownCtrl = ShutdownCtrl::new();
    pub static ref SHUTDOWN_ACKS: ShutdownAcks = ShutdownAcks::new();
}

static RUNTIME_STATUS: OnceLock<RuntimeStatus> = OnceLock::new();

fn init_rt_status() -> &'static RuntimeStatus {
    RUNTIME_STATUS.get_or_init(|| RuntimeStatus::new())
}

pub fn get_rt_status() -> &'static RuntimeStatus {
    init_rt_status()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownCause {
    SigInt,
    SigTerm,
    UserExit,
    StdinEof,
    StdinError,
    NoSessions,
    Other,
}

pub struct ShutdownCtrl {
    tx: Mutex<watch::Sender<bool>>,
    rx: Mutex<watch::Receiver<bool>>,
    state: Mutex<Option<ShutdownCause>>, // None until first trigger
}

impl ShutdownCtrl {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        ShutdownCtrl {
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
            state: Mutex::new(None),
        }
    }

    // Get a new receiver
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.rx.lock().unwrap().clone()
    }

    // Trigger shutdown once; returns true if this call won
    pub fn trigger_once(&self, cause: ShutdownCause) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.is_some() {
            return false;
        }
        *state = Some(cause);
        let _ = self.tx.lock().unwrap().send(true);
        true
    }

    pub fn cause(&self) -> Option<ShutdownCause> {
        *self.state.lock().unwrap()
    }
}

#[inline]
pub async fn wait_for_exit() {
    let mut mgr_sig = SHUTDOWN_SIGNAL.subscribe();
    match mgr_sig.changed().await {
        Ok(_) => {}
        Err(e) => {
            error!("Error: {}", e);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Component {
    CmdFlow,
    DbgMgr,
    Api,
}

pub struct ShutdownAcks {
    senders: Mutex<HashMap<Component, oneshot::Sender<()>>>,
}

impl ShutdownAcks {
    pub fn new() -> Self {
        ShutdownAcks {
            senders: Mutex::new(HashMap::new()),
        }
    }

    // Register a component to await its completion. Caller keeps the receiver.
    pub fn register(&self, component: Component) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.senders.lock().unwrap().insert(component, tx);
        rx
    }

    // Acknowledge completion for a component (idempotent best-effort)
    pub fn ack(&self, component: Component) {
        if let Some(tx) = self.senders.lock().unwrap().remove(&component) {
            let _ = tx.send(());
        }
    }
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
        for component in &[Component::CmdFlow, Component::DbgMgr] {
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
        debug!("Component {:?} is up.", component);

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
                info!("Runtime is up.");
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
