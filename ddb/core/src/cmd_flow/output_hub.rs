use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::SystemTime,
};

use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DebuggerOutputStream {
    Console,
    Log,
    Target,
    InferiorStdout,
    InferiorStderr,
    Prompt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DebuggerOutputRecord {
    pub(crate) sequence: u64,
    pub(crate) observed_at: SystemTime,
    pub(crate) session_id: Option<u64>,
    pub(crate) stream: DebuggerOutputStream,
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DebuggerOutputGap {
    pub(crate) first_missing_sequence: u64,
    pub(crate) last_missing_sequence: u64,
    pub(crate) dropped_events: u64,
    pub(crate) dropped_bytes: Option<u64>,
    pub(crate) reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OutputDelivery {
    Record(Arc<DebuggerOutputRecord>),
    Gap(DebuggerOutputGap),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OutputHubConfig {
    pub(crate) subscriber_queue: usize,
    pub(crate) max_subscribers: usize,
    pub(crate) max_text_bytes: usize,
}

impl Default for OutputHubConfig {
    fn default() -> Self {
        Self {
            subscriber_queue: 2_048,
            max_subscribers: 64,
            max_text_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum OutputHubError {
    #[error("output hub is closed")]
    Closed,
    #[error("maximum concurrent output subscribers reached")]
    SubscriberLimit,
    #[error("output sequence is exhausted")]
    SequenceExhausted,
}

#[derive(Default)]
struct OutputState {
    sequence: u64,
}

pub(crate) struct OutputHub {
    config: OutputHubConfig,
    state: Mutex<OutputState>,
    live: broadcast::Sender<Arc<DebuggerOutputRecord>>,
    shutdown: watch::Sender<bool>,
    subscribers: AtomicUsize,
    closed: AtomicBool,
}

impl OutputHub {
    pub(crate) fn new(config: OutputHubConfig) -> Arc<Self> {
        assert!(config.subscriber_queue > 0);
        assert!(config.max_subscribers > 0);
        assert!(config.max_text_bytes > 0);
        let (live, _) = broadcast::channel(config.subscriber_queue);
        let (shutdown, _) = watch::channel(false);
        Arc::new(Self {
            config,
            state: Mutex::new(OutputState::default()),
            live,
            shutdown,
            subscribers: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        })
    }

    fn state(&self) -> MutexGuard<'_, OutputState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn publish(
        &self,
        session_id: Option<u64>,
        stream: DebuggerOutputStream,
        text: impl Into<String>,
    ) -> Result<(), OutputHubError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(OutputHubError::Closed);
        }
        let (text, truncated) = truncate_utf8(text.into(), self.config.max_text_bytes);
        let mut state = self.state();
        let sequence = state
            .sequence
            .checked_add(1)
            .ok_or(OutputHubError::SequenceExhausted)?;
        state.sequence = sequence;
        let record = Arc::new(DebuggerOutputRecord {
            sequence,
            observed_at: SystemTime::now(),
            session_id,
            stream,
            text,
            truncated,
        });
        let _ = self.live.send(record);
        Ok(())
    }

    pub(crate) fn current_sequence(&self) -> u64 {
        self.state().sequence
    }

    pub(crate) fn subscribe(
        self: &Arc<Self>,
        after_sequence: Option<u64>,
    ) -> Result<OutputSubscription, OutputHubError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(OutputHubError::Closed);
        }
        self.subscribers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.config.max_subscribers).then_some(current + 1)
            })
            .map_err(|_| OutputHubError::SubscriberLimit)?;

        // Subscribing and sampling the baseline under the same lock as publish
        // gives admission one precise linearization point.
        let state = self.state();
        let live = self.live.subscribe();
        let current = state.sequence;
        drop(state);
        let pending_gap = after_sequence
            .filter(|after| *after < current)
            .map(|after| DebuggerOutputGap {
                first_missing_sequence: after.saturating_add(1),
                last_missing_sequence: current,
                dropped_events: current.saturating_sub(after),
                dropped_bytes: None,
                reason: "output replay is not retained",
            });
        let (delivery_tx, delivery_rx) = mpsc::channel(self.config.subscriber_queue);
        let task = tokio::spawn(pump_output_subscription(
            live,
            self.shutdown.subscribe(),
            delivery_tx,
            pending_gap,
            current,
            after_sequence.unwrap_or(current),
        ));
        Ok(OutputSubscription {
            hub: Arc::clone(self),
            delivery_rx,
            task,
        })
    }

    pub(crate) fn shutdown(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.shutdown.send(true);
        }
    }
}

pub(crate) struct OutputSubscription {
    hub: Arc<OutputHub>,
    delivery_rx: mpsc::Receiver<OutputDelivery>,
    task: JoinHandle<()>,
}

impl OutputSubscription {
    pub(crate) async fn recv(&mut self) -> Option<OutputDelivery> {
        self.delivery_rx.recv().await
    }
}

impl Drop for OutputSubscription {
    fn drop(&mut self) {
        self.task.abort();
        self.hub.subscribers.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn pump_output_subscription(
    mut live: broadcast::Receiver<Arc<DebuggerOutputRecord>>,
    mut shutdown: watch::Receiver<bool>,
    delivery_tx: mpsc::Sender<OutputDelivery>,
    mut pending_gap: Option<DebuggerOutputGap>,
    skip_through: u64,
    mut last_sequence: u64,
) {
    loop {
        if pending_gap.is_some() {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                permit = delivery_tx.reserve() => {
                    let Ok(permit) = permit else {
                        return;
                    };
                    permit.send(OutputDelivery::Gap(
                        pending_gap.take().expect("pending output gap exists"),
                    ));
                }
                delivery = live.recv() => {
                    if !accumulate_output_delivery(
                        delivery,
                        skip_through,
                        &mut last_sequence,
                        &mut pending_gap,
                    ) {
                        return;
                    }
                }
            }
            continue;
        }

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            delivery = live.recv() => {
                match delivery {
                    Ok(record) if record.sequence <= skip_through => {}
                    Ok(record) => {
                        last_sequence = record.sequence;
                        match delivery_tx.try_send(OutputDelivery::Record(Arc::clone(&record))) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                pending_gap = Some(gap_for_record(&record));
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let first = last_sequence.saturating_add(1);
                        let last = last_sequence.saturating_add(skipped);
                        last_sequence = last;
                        pending_gap = Some(DebuggerOutputGap {
                            first_missing_sequence: first,
                            last_missing_sequence: last,
                            dropped_events: skipped,
                            dropped_bytes: None,
                            reason: "output ingress queue overflowed",
                        });
                    }
                }
            }
        }
    }
}

fn accumulate_output_delivery(
    delivery: Result<Arc<DebuggerOutputRecord>, broadcast::error::RecvError>,
    skip_through: u64,
    last_sequence: &mut u64,
    pending_gap: &mut Option<DebuggerOutputGap>,
) -> bool {
    match delivery {
        Ok(record) if record.sequence <= skip_through => true,
        Ok(record) => {
            *last_sequence = record.sequence;
            extend_gap_with_record(
                pending_gap.as_mut().expect("pending output gap exists"),
                &record,
            );
            true
        }
        Err(broadcast::error::RecvError::Closed) => false,
        Err(broadcast::error::RecvError::Lagged(skipped)) => {
            let first = last_sequence.saturating_add(1);
            let last = last_sequence.saturating_add(skipped);
            *last_sequence = last;
            let gap = pending_gap.as_mut().expect("pending output gap exists");
            gap.last_missing_sequence = last;
            gap.dropped_events = gap.dropped_events.saturating_add(skipped);
            gap.dropped_bytes = None;
            if gap.first_missing_sequence > first {
                gap.first_missing_sequence = first;
            }
            true
        }
    }
}

fn gap_for_record(record: &DebuggerOutputRecord) -> DebuggerOutputGap {
    DebuggerOutputGap {
        first_missing_sequence: record.sequence,
        last_missing_sequence: record.sequence,
        dropped_events: 1,
        dropped_bytes: Some(record.text.len() as u64),
        reason: "output subscriber queue overflowed",
    }
}

fn extend_gap_with_record(gap: &mut DebuggerOutputGap, record: &DebuggerOutputRecord) {
    gap.last_missing_sequence = record.sequence;
    gap.dropped_events = gap.dropped_events.saturating_add(1);
    gap.dropped_bytes = gap
        .dropped_bytes
        .map(|bytes| bytes.saturating_add(record.text.len() as u64));
}

fn truncate_utf8(mut text: String, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut boundary = max_bytes;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    (text, true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn config(queue: usize) -> OutputHubConfig {
        OutputHubConfig {
            subscriber_queue: queue,
            max_subscribers: 1,
            max_text_bytes: 4,
        }
    }

    #[tokio::test]
    async fn subscriber_overflow_is_reported_without_blocking_publishers() {
        let hub = OutputHub::new(config(2));
        let mut subscription = hub.subscribe(None).unwrap();
        for value in 0..8 {
            hub.publish(None, DebuggerOutputStream::Console, value.to_string())
                .unwrap();
        }
        let OutputDelivery::Gap(gap) = subscription.recv().await.unwrap() else {
            panic!("slow subscriber must receive an explicit gap");
        };
        assert_eq!(gap.first_missing_sequence, 1);
        assert!(gap.last_missing_sequence >= 6);
        assert!(gap.dropped_events >= 6);
    }

    #[tokio::test]
    async fn unavailable_replay_is_an_initial_gap_then_live_output() {
        let hub = OutputHub::new(config(4));
        hub.publish(Some(7), DebuggerOutputStream::Target, "past")
            .unwrap();
        let mut subscription = hub.subscribe(Some(0)).unwrap();
        assert!(matches!(
            subscription.recv().await,
            Some(OutputDelivery::Gap(DebuggerOutputGap {
                first_missing_sequence: 1,
                last_missing_sequence: 1,
                ..
            }))
        ));
        hub.publish(Some(7), DebuggerOutputStream::Target, "ééé")
            .unwrap();
        let Some(OutputDelivery::Record(record)) = subscription.recv().await else {
            panic!("live output must follow the replay gap");
        };
        assert_eq!(record.sequence, 2);
        assert_eq!(record.text, "éé");
        assert!(record.truncated);
    }

    #[test]
    fn known_subscriber_queue_drops_accumulate_exact_bytes() {
        let first = DebuggerOutputRecord {
            sequence: 4,
            observed_at: SystemTime::UNIX_EPOCH,
            session_id: None,
            stream: DebuggerOutputStream::Console,
            text: "ab".to_string(),
            truncated: false,
        };
        let second = DebuggerOutputRecord {
            sequence: 5,
            text: "cde".to_string(),
            ..first.clone()
        };

        let mut gap = gap_for_record(&first);
        extend_gap_with_record(&mut gap, &second);

        assert_eq!(gap.first_missing_sequence, 4);
        assert_eq!(gap.last_missing_sequence, 5);
        assert_eq!(gap.dropped_events, 2);
        assert_eq!(gap.dropped_bytes, Some(5));
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_make_a_fast_subscriber_lose_output() {
        let hub = OutputHub::new(OutputHubConfig {
            subscriber_queue: 2,
            max_subscribers: 2,
            max_text_bytes: 4,
        });
        let mut slow = hub.subscribe(None).unwrap();
        let mut fast = hub.subscribe(None).unwrap();

        for sequence in 1..=8 {
            hub.publish(None, DebuggerOutputStream::Console, sequence.to_string())
                .unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            let Some(OutputDelivery::Record(record)) = fast.recv().await else {
                panic!("fast subscriber must receive every record");
            };
            assert_eq!(record.sequence, sequence);
        }

        for expected in 1..=2 {
            let Some(OutputDelivery::Record(record)) = slow.recv().await else {
                panic!("records queued before overflow must remain ordered");
            };
            assert_eq!(record.sequence, expected);
        }
        let Some(OutputDelivery::Gap(gap)) = slow.recv().await else {
            panic!("slow subscriber must receive one aggregated gap");
        };
        assert_eq!(gap.first_missing_sequence, 3);
        assert_eq!(gap.last_missing_sequence, 8);
        assert_eq!(gap.dropped_events, 6);
        assert_eq!(gap.dropped_bytes, Some(6));
    }

    #[tokio::test]
    async fn subscriber_limit_and_shutdown_are_bounded() {
        let hub = OutputHub::new(config(2));
        let mut subscription = hub.subscribe(None).unwrap();
        assert!(matches!(
            hub.subscribe(None),
            Err(OutputHubError::SubscriberLimit)
        ));
        hub.shutdown();
        assert!(subscription.recv().await.is_none());
        assert_eq!(
            hub.publish(None, DebuggerOutputStream::Log, "closed")
                .unwrap_err(),
            OutputHubError::Closed
        );
    }
}
