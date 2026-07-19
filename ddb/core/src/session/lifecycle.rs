use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tokio::sync::mpsc;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum SessionTerminationCause {
    ProtocolExit { reasons: Vec<String> },
    ProtocolFault { message: String },
    TransportExited { status: Option<u32> },
    TransportFault { message: String },
    EventStreamClosed,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SessionTermination {
    pub sid: u64,
    pub cause: SessionTerminationCause,
}

#[derive(Clone)]
pub(crate) struct SessionLifecycleHandle {
    sender: mpsc::UnboundedSender<SessionTermination>,
}

impl SessionLifecycleHandle {
    pub(crate) fn bind(&self, sid: u64) -> SessionTerminationReporter {
        SessionTerminationReporter {
            sid,
            sender: self.sender.clone(),
            termination_requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SessionTerminationReporter {
    sid: u64,
    sender: mpsc::UnboundedSender<SessionTermination>,
    termination_requested: Arc<AtomicBool>,
}

impl SessionTerminationReporter {
    pub(crate) fn terminate(&self, cause: SessionTerminationCause) {
        if self
            .termination_requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = self.sender.send(SessionTermination {
                sid: self.sid,
                cause,
            });
        }
    }

    pub(crate) fn termination_requested(&self) -> bool {
        self.termination_requested.load(Ordering::Acquire)
    }
}

pub(crate) fn channel() -> (
    SessionLifecycleHandle,
    mpsc::UnboundedReceiver<SessionTermination>,
) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (SessionLifecycleHandle { sender }, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reporter_emits_only_the_first_terminal_cause() {
        let (lifecycle, mut terminations) = channel();
        let reporter = lifecycle.bind(17);

        reporter.terminate(SessionTerminationCause::TransportFault {
            message: "first".into(),
        });
        reporter.terminate(SessionTerminationCause::EventStreamClosed);

        assert!(reporter.termination_requested());
        assert_eq!(
            terminations.try_recv().unwrap(),
            SessionTermination {
                sid: 17,
                cause: SessionTerminationCause::TransportFault {
                    message: "first".into(),
                },
            }
        );
        assert!(terminations.try_recv().is_err());
    }
}
