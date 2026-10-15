use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

/// Orders explicit cancellation against the final automatic write admission.
/// The gate is held only for synchronous work, never debugger I/O or an await.
#[derive(Default)]
pub(crate) struct ExecutionControl {
    epoch: AtomicU64,
    retirement_floor: AtomicU64,
    admission: Mutex<()>,
    explicit_pauses: Mutex<HashMap<u64, u64>>,
}

impl ExecutionControl {
    pub(crate) fn current(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn advance(&self) {
        self.advance_for_pauses(&[]);
    }

    pub(crate) fn retirement_floor(&self) -> u64 {
        self.retirement_floor.load(Ordering::SeqCst)
    }

    /// A mode change cancels work while allowing current native stops to be
    /// classified in the new scope. Explicit intent additionally retires them.
    pub(crate) fn reconfigure(&self) {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn advance_for_pauses(&self, sessions: &[u64]) {
        self.advance_explicit(sessions, &[]);
    }

    pub(crate) fn advance_for_resumes(&self, sessions: &[u64]) {
        self.advance_explicit(&[], sessions);
    }

    fn advance_explicit(&self, pause_sessions: &[u64], resume_sessions: &[u64]) {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut pauses = self
            .explicit_pauses
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for session in resume_sessions {
            pauses.remove(session);
        }
        let epoch = self.current() + 1;
        // A classifier that observes the new epoch must also see its floor.
        self.retirement_floor.store(epoch, Ordering::SeqCst);
        self.epoch.store(epoch, Ordering::SeqCst);
        for session in pause_sessions {
            pauses.insert(*session, epoch);
        }
    }

    pub(crate) fn take_explicit_pause(&self, session: u64, observed_epoch: u64) -> Option<u64> {
        let mut pauses = self
            .explicit_pauses
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pauses
            .get(&session)
            .is_some_and(|epoch| *epoch <= observed_epoch)
        {
            pauses.remove(&session)
        } else {
            None
        }
    }

    pub(crate) fn has_explicit_pause(&self, session: u64, observed_epoch: u64) -> bool {
        self.explicit_pauses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&session)
            .is_some_and(|epoch| *epoch <= observed_epoch)
    }

    pub(crate) fn forget_session(&self, session: u64) {
        self.explicit_pauses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&session);
    }

    pub(crate) fn admit<T>(&self, expected: u64, action: impl FnOnce() -> T) -> Option<T> {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (self.current() == expected).then(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{mpsc, Arc},
        time::Duration,
    };

    #[test]
    fn mode_changes_preserve_the_explicit_retirement_floor() {
        let control = ExecutionControl::default();
        control.reconfigure();
        assert_eq!(control.current(), 1);
        assert_eq!(control.retirement_floor(), 0);
        control.advance_for_pauses(&[42]);
        assert_eq!(control.current(), 2);
        assert_eq!(control.retirement_floor(), 2);
        control.reconfigure();
        assert_eq!(control.current(), 3);
        assert_eq!(control.retirement_floor(), 2);
        assert_eq!(control.take_explicit_pause(42, 3), Some(2));
    }

    #[test]
    fn resumes_supersede_only_their_targets_unobserved_pauses() {
        let control = ExecutionControl::default();
        control.advance_for_pauses(&[42, 43]);
        control.advance_for_resumes(&[42]);
        assert!(!control.has_explicit_pause(42, control.current()));
        assert!(control.has_explicit_pause(43, control.current()));
        control.advance_for_pauses(&[]);
        control.reconfigure();
        assert_eq!(control.take_explicit_pause(43, control.current()), Some(1));
        assert_eq!(control.retirement_floor(), 3);
    }

    #[test]
    fn cancellation_and_admission_have_a_single_order_across_threads() {
        let control = Arc::new(ExecutionControl::default());
        let (entered, ready) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let (cancel_started, cancelling) = mpsc::channel();
        let (cancel_done, cancelled) = mpsc::channel();
        let writer_control = control.clone();
        let writer = std::thread::spawn(move || {
            writer_control.admit(0, || {
                entered.send(()).unwrap();
                proceed.recv_timeout(Duration::from_secs(1)).unwrap();
            })
        });
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        let cancel_control = control.clone();
        let canceller = std::thread::spawn(move || {
            cancel_started.send(()).unwrap();
            cancel_control.advance();
            cancel_done.send(()).unwrap();
        });
        cancelling.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cancelled.recv_timeout(Duration::from_millis(20)).is_err());
        release.send(()).unwrap();
        assert!(writer.join().unwrap().is_some());
        canceller.join().unwrap();
        assert_eq!(control.current(), 1);
        assert!(control
            .admit(0, || panic!("cancelled generation must not write"))
            .is_none());
    }
}
