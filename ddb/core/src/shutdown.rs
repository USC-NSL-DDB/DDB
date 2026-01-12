use nix::sys::signal::{pthread_sigmask, SigSet, SigmaskHow, Signal};
use std::{
    collections::HashMap,
    future::IntoFuture,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch},
    time::timeout,
};
use tracing::{debug, error, info, warn};

use crate::status::Component;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

static SHUTDOWN_CTRL: OnceLock<ShutdownCtrl> = OnceLock::new();
static SHUTDOWN_ACKS: OnceLock<ShutdownAcks> = OnceLock::new();

/// Returns the global shutdown controller instance.
pub fn get_shutdown_ctrl() -> &'static ShutdownCtrl {
    SHUTDOWN_CTRL.get_or_init(|| ShutdownCtrl::new())
}

fn get_shutdown_acks() -> &'static ShutdownAcks {
    SHUTDOWN_ACKS.get_or_init(|| ShutdownAcks::new())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownCause {
    SigInt,
    SigTerm,
    UserExit,
    StdinEof,
    StdinError,
    NoSessions,
    DbgMgrInitFailure,
    Other,
}

pub struct ShutdownCtrl {
    tx: Mutex<watch::Sender<bool>>,
    rx: Mutex<watch::Receiver<bool>>,
    state: Mutex<Option<ShutdownCause>>, // None until first trigger
    shutdown_ack_pending: Mutex<Vec<(Component, oneshot::Receiver<()>)>>,
    signal_handling_thrd: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ShutdownCtrl {
    #[inline]
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        ShutdownCtrl {
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
            state: Mutex::new(None),
            shutdown_ack_pending: Mutex::new(vec![]),
            signal_handling_thrd: Mutex::new(None),
        }
    }

    /// Waits asynchronously until a shutdown signal is triggered.
    ///
    /// This function subscribes to the global shutdown controller and blocks until
    /// a shutdown event occurs (e.g., SIGINT, SIGTERM, or user exit).
    /// It returns immediately once the shutdown signal is received.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use ddb::shutdown::ShutdownCtrl;
    /// # async fn example() {
    /// ShutdownCtrl::wait_for_exit().await;
    /// println!("Shutdown signal received, cleaning up...");
    /// # }
    /// ```
    #[inline]
    pub async fn wait_for_exit() {
        let mut mgr_sig = get_shutdown_ctrl().subscribe();
        match mgr_sig.changed().await {
            Ok(_) => {}
            Err(e) => {
                error!("Error: {}", e);
            }
        }
    }
}

impl ShutdownCtrl {
    /// Subscribes to shutdown notifications.
    ///
    /// Returns a receiver that will be notified when shutdown is triggered.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.rx.lock().unwrap().clone()
    }

    /// Triggers shutdown with the given cause.
    ///
    /// Returns `true` if this call initiated the shutdown, `false` if shutdown was already triggered.
    /// Only the first call to this function will succeed; subsequent calls are no-ops.
    pub fn trigger_once(&self, cause: ShutdownCause) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.is_some() {
            return false;
        }
        *state = Some(cause);
        let _ = self.tx.lock().unwrap().send(true);
        true
    }

    /// Returns the cause of shutdown, or `None` if shutdown has not been triggered.
    pub fn cause(&self) -> Option<ShutdownCause> {
        *self.state.lock().unwrap()
    }

    /// Registers a component that must acknowledge shutdown before completion.
    pub fn register_ack(&self, component: Component) {
        let receiver = get_shutdown_acks().register(component);
        self.shutdown_ack_pending
            .lock()
            .unwrap()
            .push((component, receiver));
    }

    /// Registers multiple components that must acknowledge shutdown before completion.
    pub fn register_acks(&self, components: &[Component]) {
        for &component in components {
            self.register_ack(component);
        }
    }

    /// Acknowledges that a component has completed its shutdown procedure.
    pub fn ack_shutdown(&self, component: Component) {
        get_shutdown_acks().ack(component);
    }

    /// Sets up signal handling for SIGINT and SIGTERM.
    ///
    /// Blocks these signals in the current thread and spawns a dedicated thread to handle them.
    /// When a signal is received, it triggers shutdown with the appropriate cause.
    pub fn setup_signal_handling(&self) {
        // block signals in this thread (propagates to children); dedicated signal thread will wait on them
        let mut sigset = SigSet::empty();
        sigset.add(Signal::SIGINT);
        sigset.add(Signal::SIGTERM);
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&sigset), None)
            .expect("failed to block signals");

        // start dedicated signal waiter thread
        let sigset_for_thread = sigset.clone();

        *self.signal_handling_thrd.lock().unwrap() = Some(std::thread::spawn(move || loop {
            match sigset_for_thread.wait() {
                Ok(Signal::SIGINT) => {
                    get_shutdown_ctrl().trigger_once(ShutdownCause::SigInt);
                    break;
                }
                Ok(Signal::SIGTERM) => {
                    get_shutdown_ctrl().trigger_once(ShutdownCause::SigTerm);
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    get_shutdown_ctrl().trigger_once(ShutdownCause::Other);
                    break;
                }
            }
        }));
    }

    /// Runs a cleanup function with a timeout.
    ///
    /// The cleanup function will be cancelled if it doesn't complete within `SHUTDOWN_TIMEOUT`.
    pub async fn shutdown_cleanup<F>(&self, cleanup_fn: F)
    where
        F: IntoFuture,
    {
        let _ = timeout(SHUTDOWN_TIMEOUT, cleanup_fn).await;
    }

    /// Blocks until shutdown is triggered, then waits for all registered components to acknowledge.
    ///
    /// Each component has `SHUTDOWN_TIMEOUT` to respond. This function uses a dedicated runtime
    /// and blocks the current thread until all acknowledgments are received or timeouts occur.
    pub fn wait_for_shutdown(&self) {
        // wait for all components to ack or timeout using a small runtime.
        let wait_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        wait_rt.block_on(async {
            let mut shutdown_sig = self.subscribe();
            match shutdown_sig.changed().await {
                Ok(_) => {
                    info!("Shutdown signal received, waiting for components to ack...");
                    for (comp, rx) in self.shutdown_ack_pending.lock().unwrap().drain(..) {
                        let res = timeout(SHUTDOWN_TIMEOUT, rx).await;
                        match res {
                            Ok(Ok(())) => debug!("Received shutdown ack from {:?}", comp),
                            Ok(Err(_)) => warn!("Shutdown ack dropped for {:?}", comp),
                            Err(_) => warn!("Timed out waiting for shutdown ack from {:?}", comp),
                        }
                    }
                }
                Err(_) => {
                    warn!("Shutdown signal channel closed unexpectedly");
                }
            }
        });
    }
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

    /// Registers a component and returns a receiver that will be signaled when the component acknowledges.
    pub fn register(&self, component: Component) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.senders.lock().unwrap().insert(component, tx);
        rx
    }

    /// Acknowledges completion for a component.
    ///
    /// This is idempotent and best-effort; if the component is not registered, this is a no-op.
    pub fn ack(&self, component: Component) {
        if let Some(tx) = self.senders.lock().unwrap().remove(&component) {
            let _ = tx.send(());
        }
    }
}
