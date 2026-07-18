use std::{future::IntoFuture, sync::Mutex, time::Duration};
use tokio::{
    signal::unix::{signal, SignalKind},
    sync::watch,
    time::timeout,
};
use tracing::error;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Returns the global shutdown controller instance.
pub fn get_shutdown_ctrl() -> &'static ShutdownCtrl {
    crate::context::app_context().shutdown()
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
    ApiServerInitFailure,
    Other,
}

pub struct ShutdownCtrl {
    tx: Mutex<watch::Sender<bool>>,
    rx: Mutex<watch::Receiver<bool>>,
    state: Mutex<Option<ShutdownCause>>, // None until first trigger
}

impl ShutdownCtrl {
    #[inline]
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        ShutdownCtrl {
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
            state: Mutex::new(None),
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

    pub fn should_shutdown(&self) -> bool {
        *self.rx.lock().unwrap().borrow()
    }

    /// Returns the cause of shutdown, or `None` if shutdown has not been triggered.
    pub fn cause(&self) -> Option<ShutdownCause> {
        *self.state.lock().unwrap()
    }

    /// Waits for SIGINT or SIGTERM, or returns when another source requests shutdown.
    ///
    /// The root task supervisor owns this future, so signal handling is joined
    /// with every other component instead of being detached.
    pub async fn wait_for_signal(&self) {
        let mut sigint =
            signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        let mut shutdown = self.subscribe();

        tokio::select! {
            _ = sigint.recv() => {
                self.trigger_once(ShutdownCause::SigInt);
            }
            _ = sigterm.recv() => {
                self.trigger_once(ShutdownCause::SigTerm);
            }
            _ = shutdown.changed() => {}
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_once_records_only_the_first_shutdown_cause() {
        let ctrl = ShutdownCtrl::new();

        assert!(ctrl.trigger_once(ShutdownCause::UserExit));
        assert!(!ctrl.trigger_once(ShutdownCause::SigInt));
        assert!(ctrl.should_shutdown());
        assert_eq!(ctrl.cause(), Some(ShutdownCause::UserExit));
    }
}
