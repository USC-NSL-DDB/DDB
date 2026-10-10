//! Opt-in bounded request metadata, never request/response data or endpoint URLs.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
use tokio::sync::broadcast;

/// One HTTP attempt's lifecycle event. IDs are shared by clones of a client.
/// Retries have distinct IDs. A response means headers arrived, not that the
/// body decoded successfully or a streamed operation completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestTrace {
    pub attempt_id: u64,
    /// Canonical service name, without endpoint prefixes or query parameters.
    pub service: &'static str,
    pub operation: &'static str,
    pub outcome: RequestTraceOutcome,
}

/// Fixed metadata only. Transport errors deliberately contain no error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestTraceOutcome {
    Started,
    Response(u16),
    TransportFailed,
    Cancelled,
}

impl std::fmt::Display for RequestTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[DDB API] {} POST /api/v2/rpc/{}/{}",
            self.attempt_id, self.service, self.operation
        )?;
        match self.outcome {
            RequestTraceOutcome::Started => Ok(()),
            RequestTraceOutcome::Response(status) => write!(f, " -> {status}"),
            RequestTraceOutcome::TransportFailed => write!(f, " -> transport failed"),
            RequestTraceOutcome::Cancelled => write!(f, " -> cancelled"),
        }
    }
}

#[derive(Default)]
pub(crate) struct TraceHub {
    sender: OnceLock<broadcast::Sender<RequestTrace>>,
    sequence: AtomicU64,
}

impl TraceHub {
    pub fn subscribe(&self) -> broadcast::Receiver<RequestTrace> {
        self.sender
            .get_or_init(|| broadcast::channel(256).0)
            .subscribe()
    }

    pub fn begin(&self, service: &'static str, operation: &'static str) -> Option<Attempt> {
        let sender = self.sender.get()?;
        if sender.receiver_count() == 0 {
            return None;
        }
        let record = RequestTrace {
            attempt_id: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            service,
            operation,
            outcome: RequestTraceOutcome::Started,
        };
        let _ = sender.send(record.clone());
        Some(Attempt {
            sender: sender.clone(),
            record,
            finished: false,
        })
    }
}

pub(crate) struct Attempt {
    sender: broadcast::Sender<RequestTrace>,
    record: RequestTrace,
    finished: bool,
}

impl Attempt {
    pub fn finish(mut self, outcome: RequestTraceOutcome) {
        self.record.outcome = outcome;
        self.finished = true;
        let _ = self.sender.send(self.record.clone());
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.finished {
            self.record.outcome = RequestTraceOutcome::Cancelled;
            let _ = self.sender.send(self.record.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tracing_is_lazy_bounded_and_reports_cancellation() {
        let hub = TraceHub::default();
        assert!(hub.begin("service", "operation").is_none());
        assert!(hub.sender.get().is_none());
        let mut receiver = hub.subscribe();
        drop(hub.begin("service", "operation"));
        let start = receiver.try_recv().unwrap();
        let end = receiver.try_recv().unwrap();
        assert_eq!(start.attempt_id, end.attempt_id);
        assert_eq!(start.outcome, RequestTraceOutcome::Started);
        assert_eq!(end.outcome, RequestTraceOutcome::Cancelled);
        for _ in 0..300 {
            drop(hub.begin("service", "operation"));
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        drop(receiver);
        assert!(hub.begin("service", "operation").is_none());
    }
}
