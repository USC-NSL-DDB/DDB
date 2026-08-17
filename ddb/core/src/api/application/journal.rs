use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use ddb_api_types::v2::{
    resource_upsert, state_event, target, Cursor, DdbErrorCode, ExtensionPayload, ResourceKind,
    StateEvent, StateEventKind, Target,
};
use prost::Message;
use tokio::sync::{broadcast, watch};

use super::{timestamp_now, ApplicationError};
use crate::api::telemetry::{
    record_replay_gap, record_state_event, record_state_journal_depth, record_subscriber_delta,
};

const SCHEMA_VERSION: &str = "2.0";

#[derive(Clone, Debug)]
pub(crate) struct StateJournalConfig {
    pub(crate) max_events: usize,
    pub(crate) max_bytes: usize,
    pub(crate) retention: Duration,
    pub(crate) subscriber_queue: usize,
    pub(crate) max_subscribers: usize,
}

impl Default for StateJournalConfig {
    fn default() -> Self {
        Self {
            max_events: 16_384,
            max_bytes: 32 * 1024 * 1024,
            retention: Duration::from_secs(10 * 60),
            subscriber_queue: 1_024,
            max_subscribers: 64,
        }
    }
}

pub(crate) struct StateChange {
    pub(crate) request_id: Option<String>,
    pub(crate) operation_id: Option<String>,
    pub(crate) kind: StateEventKind,
    pub(crate) resource_kind: ResourceKind,
    pub(crate) resource_id: String,
    pub(crate) resource_revision: u64,
    pub(crate) payload: state_event::Payload,
    pub(crate) extension_details: Vec<ExtensionPayload>,
    pub(crate) context: StateEventContext,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StateEventContext {
    pub(crate) global: bool,
    pub(crate) session_ids: Vec<String>,
    pub(crate) group_ids: Vec<String>,
}

impl StateEventContext {
    pub(crate) fn merge(&mut self, other: &Self) {
        self.global |= other.global;
        self.session_ids.extend(other.session_ids.iter().cloned());
        self.group_ids.extend(other.group_ids.iter().cloned());
        self.session_ids.sort_unstable();
        self.session_ids.dedup();
        self.group_ids.sort_unstable();
        self.group_ids.dedup();
    }

    pub(crate) fn from_resource(resource: &resource_upsert::Resource) -> Self {
        let mut context = Self::default();
        match resource {
            resource_upsert::Resource::Session(resource) => {
                context.session_ids.push(resource.session_id.clone());
                context.group_ids.extend(resource.group_id.iter().cloned());
            }
            resource_upsert::Resource::Group(resource) => {
                context.group_ids.push(resource.group_id.clone());
                context
                    .session_ids
                    .extend(resource.session_ids.iter().cloned());
            }
            resource_upsert::Resource::Process(resource) => {
                context.session_ids.push(resource.session_id.clone());
                context.group_ids.extend(resource.group_id.iter().cloned());
            }
            resource_upsert::Resource::Thread(resource) => {
                context.session_ids.push(resource.session_id.clone());
                context.group_ids.extend(resource.group_id.iter().cloned());
            }
            resource_upsert::Resource::Selection(resource) => {
                context
                    .session_ids
                    .extend(resource.session_id.iter().cloned());
                context.group_ids.extend(resource.group_id.iter().cloned());
            }
            resource_upsert::Resource::ExecutionState(resource) => {
                if let Some(target) = resource.target.as_ref() {
                    context.add_target(target);
                }
            }
            resource_upsert::Resource::Breakpoint(resource) => {
                if let Some(target) = resource.target.as_ref() {
                    context.add_target(target);
                }
                context.session_ids.extend(
                    resource
                        .sub_breakpoints
                        .iter()
                        .map(|sub| sub.session_id.clone()),
                );
                context.group_ids.extend(
                    resource
                        .sub_breakpoints
                        .iter()
                        .filter_map(|sub| sub.inherited_from_group_id.clone()),
                );
            }
            resource_upsert::Resource::Operation(resource) => {
                if let Some(target) = resource
                    .target
                    .as_ref()
                    .and_then(|summary| summary.target.as_ref())
                {
                    context.add_target(target);
                }
                for outcome in &resource.target_outcomes {
                    if let Some(target) = outcome.target.as_ref() {
                        context.add_target(target);
                    }
                }
            }
            resource_upsert::Resource::PendingCommand(resource) => {
                context.session_ids.push(resource.session_id.clone());
            }
            resource_upsert::Resource::Capabilities(_)
            | resource_upsert::Resource::ExtensionState(_) => context.global = true,
        }
        context.normalize();
        context
    }

    pub(crate) fn add_session(&mut self, session_id: String) {
        self.session_ids.push(session_id);
        self.normalize();
    }

    pub(crate) fn add_group(&mut self, group_id: String) {
        self.group_ids.push(group_id);
        self.normalize();
    }

    pub(crate) fn mark_global(&mut self) {
        self.global = true;
    }

    fn add_target(&mut self, target: &Target) {
        match target.selector.as_ref() {
            Some(target::Selector::Session(target)) => {
                self.session_ids.push(target.session_id.clone())
            }
            Some(target::Selector::Group(target)) => self.group_ids.push(target.group_id.clone()),
            Some(target::Selector::SessionSet(target)) => {
                self.session_ids.extend(target.session_ids.iter().cloned())
            }
            Some(target::Selector::Multiple(target)) => {
                for target in &target.targets {
                    self.add_target(target);
                }
            }
            Some(target::Selector::Broadcast(_)) => self.global = true,
            _ => {}
        }
    }

    fn normalize(&mut self) {
        self.session_ids.sort_unstable();
        self.session_ids.dedup();
        self.group_ids.sort_unstable();
        self.group_ids.dedup();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct JournalEvent {
    pub(crate) event: StateEvent,
    pub(crate) context: StateEventContext,
}

struct RetainedEvent {
    envelope: JournalEvent,
    encoded_bytes: usize,
    retained_at: Instant,
}

#[derive(Default)]
struct JournalState {
    sequence: u64,
    state_revision: u64,
    retained_bytes: usize,
    events: VecDeque<RetainedEvent>,
}

struct JournalInner {
    server_instance_id: String,
    config: StateJournalConfig,
    state: Mutex<JournalState>,
    live: broadcast::Sender<JournalEvent>,
    shutdown: watch::Sender<bool>,
    subscribers: AtomicUsize,
    closed: AtomicBool,
}

/// Ordered bounded journal for replayable client-visible state.
#[derive(Clone)]
pub(crate) struct StateJournal {
    inner: Arc<JournalInner>,
}

impl StateJournal {
    pub(crate) fn new(server_instance_id: impl Into<String>, config: StateJournalConfig) -> Self {
        assert!(config.max_events > 0);
        assert!(config.max_bytes > 0);
        assert!(config.subscriber_queue > 0);
        assert!(config.max_subscribers > 0);
        let (live, _) = broadcast::channel(config.subscriber_queue);
        let (shutdown, _) = watch::channel(false);
        Self {
            inner: Arc::new(JournalInner {
                server_instance_id: server_instance_id.into(),
                config,
                state: Mutex::new(JournalState::default()),
                live,
                shutdown,
                subscribers: AtomicUsize::new(0),
                closed: AtomicBool::new(false),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, JournalState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn checkpoint(&self) -> (Cursor, u64) {
        let mut state = self.state();
        self.prune(&mut state, Instant::now());
        record_state_journal_depth(state.events.len(), state.retained_bytes);
        (self.cursor(state.sequence), state.state_revision)
    }

    /// Publishes only after the caller has committed the represented domain
    /// mutation. The synchronous broadcast send cannot block debugger work.
    pub(crate) fn publish(&self, change: StateChange) -> Result<StateEvent, ApplicationError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ApplicationError::new(
                DdbErrorCode::Unavailable,
                "state journal is shutting down",
            ));
        }
        let required_resync = change.kind == StateEventKind::RequiredResync
            && matches!(&change.payload, state_event::Payload::RequiredResync(_));
        if !required_resync && (change.resource_id.is_empty() || change.resource_revision == 0) {
            return Err(ApplicationError::invalid(
                "state_change.resource",
                "resource identity and revision are required",
            ));
        }

        let now = Instant::now();
        let mut state = self.state();
        self.prune(&mut state, now);
        let sequence = state.sequence.checked_add(1).ok_or_else(|| {
            ApplicationError::new(DdbErrorCode::Internal, "state sequence is exhausted")
        })?;
        let state_revision = state.state_revision.checked_add(1).ok_or_else(|| {
            ApplicationError::new(DdbErrorCode::Internal, "state revision is exhausted")
        })?;
        let event = StateEvent {
            cursor: Some(self.cursor(sequence)),
            state_revision,
            schema_version: SCHEMA_VERSION.to_string(),
            occurred_at: Some(timestamp_now()),
            request_id: change.request_id,
            operation_id: change.operation_id,
            kind: change.kind as i32,
            resource_kind: change.resource_kind as i32,
            resource_id: change.resource_id,
            resource_revision: change.resource_revision,
            payload: Some(change.payload),
            extension_details: change.extension_details,
        };
        let encoded_bytes = event.encoded_len();
        if encoded_bytes > self.inner.config.max_bytes {
            return Err(ApplicationError::resource_exhausted(
                "one state event exceeds journal byte capacity",
            ));
        }

        while state.events.len() >= self.inner.config.max_events
            || state.retained_bytes.saturating_add(encoded_bytes) > self.inner.config.max_bytes
        {
            self.pop_front(&mut state);
        }
        state.sequence = sequence;
        state.state_revision = state_revision;
        state.retained_bytes += encoded_bytes;
        let envelope = JournalEvent {
            event: event.clone(),
            context: change.context,
        };
        state.events.push_back(RetainedEvent {
            envelope: envelope.clone(),
            encoded_bytes,
            retained_at: now,
        });
        record_state_journal_depth(state.events.len(), state.retained_bytes);
        drop(state);
        let _ = self.inner.live.send(envelope);
        record_state_event(change.kind, encoded_bytes);
        Ok(event)
    }

    pub(crate) fn subscribe(
        &self,
        after: Option<&Cursor>,
    ) -> Result<StateSubscription, ApplicationError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(ApplicationError::new(
                DdbErrorCode::Unavailable,
                "state journal is shutting down",
            ));
        }
        self.reserve_subscriber()?;

        let result: Result<StateSubscription, ApplicationError> = (|| {
            let mut state = self.state();
            self.prune(&mut state, Instant::now());
            record_state_journal_depth(state.events.len(), state.retained_bytes);
            self.validate_cursor(&state, after)?;
            let live = self.inner.live.subscribe();
            let replay =
                after
                    .map(|cursor| {
                        state
                            .events
                            .iter()
                            .filter(|retained| {
                                retained.envelope.event.cursor.as_ref().is_some_and(
                                    |event_cursor| event_cursor.sequence > cursor.sequence,
                                )
                            })
                            .map(|retained| retained.envelope.clone())
                            .collect()
                    })
                    .unwrap_or_default();
            Ok(StateSubscription {
                journal: self.clone(),
                replay,
                live,
                shutdown: self.inner.shutdown.subscribe(),
            })
        })();
        if result.is_err() {
            self.release_subscriber();
            if result
                .as_ref()
                .is_err_and(|error| error.code() == DdbErrorCode::ReplayGap)
            {
                record_replay_gap("state");
            }
        }
        result
    }

    pub(crate) fn shutdown(&self) {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let _ = self.inner.shutdown.send(true);
        }
    }

    fn validate_cursor(
        &self,
        state: &JournalState,
        after: Option<&Cursor>,
    ) -> Result<(), ApplicationError> {
        let Some(after) = after else {
            return Ok(());
        };
        if after.server_instance_id != self.inner.server_instance_id {
            return Err(self.replay_gap(state));
        }
        if after.sequence > state.sequence {
            return Err(ApplicationError::invalid(
                "after_cursor.sequence",
                "is ahead of the current state cursor",
            ));
        }
        let earliest_replayable_after = state
            .events
            .front()
            .and_then(|event| event.envelope.event.cursor.as_ref())
            .map_or(state.sequence, |cursor| cursor.sequence.saturating_sub(1));
        if after.sequence < earliest_replayable_after {
            return Err(self.replay_gap(state));
        }
        Ok(())
    }

    fn replay_gap(&self, state: &JournalState) -> ApplicationError {
        let earliest_after = state
            .events
            .front()
            .and_then(|event| event.envelope.event.cursor.as_ref())
            .map_or(state.sequence, |cursor| cursor.sequence.saturating_sub(1));
        ApplicationError::new(
            DdbErrorCode::ReplayGap,
            "requested state history is no longer retained",
        )
        .with_replay_bounds(self.cursor(earliest_after), self.cursor(state.sequence))
    }

    fn reserve_subscriber(&self) -> Result<(), ApplicationError> {
        self.inner
            .subscribers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.inner.config.max_subscribers).then_some(current + 1)
            })
            .map(|_| ())
            .inspect(|_| record_subscriber_delta("state", 1))
            .map_err(|_| {
                ApplicationError::resource_exhausted("maximum concurrent state subscribers reached")
                    .retryable(true)
            })
    }

    fn release_subscriber(&self) {
        self.inner.subscribers.fetch_sub(1, Ordering::AcqRel);
        record_subscriber_delta("state", -1);
    }

    fn prune(&self, state: &mut JournalState, now: Instant) {
        while state.events.front().is_some_and(|event| {
            now.saturating_duration_since(event.retained_at) >= self.inner.config.retention
        }) {
            self.pop_front(state);
        }
    }

    fn pop_front(&self, state: &mut JournalState) {
        if let Some(event) = state.events.pop_front() {
            state.retained_bytes = state.retained_bytes.saturating_sub(event.encoded_bytes);
        }
    }

    fn cursor(&self, sequence: u64) -> Cursor {
        Cursor {
            server_instance_id: self.inner.server_instance_id.clone(),
            sequence,
        }
    }
}

pub(crate) struct StateSubscription {
    journal: StateJournal,
    replay: VecDeque<JournalEvent>,
    live: broadcast::Receiver<JournalEvent>,
    shutdown: watch::Receiver<bool>,
}

impl StateSubscription {
    pub(crate) async fn recv(&mut self) -> Result<Option<JournalEvent>, ApplicationError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(event));
        }
        loop {
            tokio::select! {
                changed = self.shutdown.changed() => {
                    if changed.is_err() || *self.shutdown.borrow() {
                        return Ok(None);
                    }
                }
                event = self.live.recv() => {
                    match event {
                        Ok(event) => return Ok(Some(event)),
                        Err(broadcast::error::RecvError::Closed) => return Ok(None),
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let state = self.journal.state();
                            return Err(self.journal.replay_gap(&state));
                        }
                    }
                }
            }
        }
    }
}

impl Drop for StateSubscription {
    fn drop(&mut self) {
        self.journal.release_subscriber();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ddb_api_types::v2::{resource_upsert, state_event, ResourceDeleted, ResourceUpsert};

    use super::*;

    fn config(max_events: usize, queue: usize) -> StateJournalConfig {
        StateJournalConfig {
            max_events,
            max_bytes: 64 * 1024,
            retention: Duration::from_secs(60),
            subscriber_queue: queue,
            max_subscribers: 2,
        }
    }

    fn change(id: &str, revision: u64) -> StateChange {
        StateChange {
            request_id: None,
            operation_id: None,
            kind: StateEventKind::ResourceUpserted,
            resource_kind: ResourceKind::Selection,
            resource_id: id.to_string(),
            resource_revision: revision,
            payload: state_event::Payload::Upsert(ResourceUpsert {
                resource: Some(resource_upsert::Resource::Selection(
                    ddb_api_types::v2::Selection {
                        selection_id: id.to_string(),
                        session_id: None,
                        group_id: None,
                        thread_id: None,
                        frame_id: None,
                        revision,
                    },
                )),
            }),
            extension_details: Vec::new(),
            context: StateEventContext::default(),
        }
    }

    fn deletion(id: &str, revision: u64) -> StateChange {
        StateChange {
            request_id: None,
            operation_id: None,
            kind: StateEventKind::ResourceDeleted,
            resource_kind: ResourceKind::Selection,
            resource_id: id.to_string(),
            resource_revision: revision,
            payload: state_event::Payload::Deleted(ResourceDeleted {
                resource_kind: ResourceKind::Selection as i32,
                resource_id: id.to_string(),
                resource_revision: revision,
            }),
            extension_details: Vec::new(),
            context: StateEventContext::default(),
        }
    }

    fn apply_revisioned_event(projection: &mut HashMap<String, (u64, bool)>, event: &StateEvent) {
        if projection
            .get(&event.resource_id)
            .is_some_and(|(revision, _)| *revision >= event.resource_revision)
        {
            return;
        }
        let present = matches!(event.payload, Some(state_event::Payload::Upsert(_)));
        projection.insert(
            event.resource_id.clone(),
            (event.resource_revision, present),
        );
    }

    fn visible_projection(projection: &HashMap<String, (u64, bool)>) -> HashMap<String, u64> {
        projection
            .iter()
            .filter(|(_, (_, present))| *present)
            .map(|(id, (revision, _))| (id.clone(), *revision))
            .collect()
    }

    #[tokio::test]
    async fn replay_handoff_is_monotonic_without_overlap() {
        let journal = StateJournal::new("instance", config(8, 8));
        let first = journal.publish(change("selection", 1)).unwrap();
        let cursor = first.cursor.clone().unwrap();
        journal.publish(change("selection", 2)).unwrap();

        let mut subscription = journal.subscribe(Some(&cursor)).unwrap();
        let replayed = subscription.recv().await.unwrap().unwrap();
        assert_eq!(replayed.event.cursor.unwrap().sequence, 2);
        journal.publish(change("selection", 3)).unwrap();
        let live = subscription.recv().await.unwrap().unwrap();
        assert_eq!(live.event.cursor.unwrap().sequence, 3);
    }

    #[tokio::test]
    async fn snapshot_replay_and_live_delivery_converge_across_five_thousand_mutations() {
        const MUTATIONS: usize = 5_000;
        const RESOURCES: usize = 64;
        const SNAPSHOT_AFTER: usize = 1_500;

        let journal = StateJournal::new(
            "instance",
            StateJournalConfig {
                max_events: MUTATIONS + 1,
                max_bytes: 16 * 1024 * 1024,
                retention: Duration::from_secs(60),
                subscriber_queue: MUTATIONS + 1,
                max_subscribers: 2,
            },
        );
        let domain = Arc::new(Mutex::new(HashMap::<String, u64>::new()));
        let (snapshot_ready_tx, snapshot_ready_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let writer_journal = journal.clone();
        let writer_domain = Arc::clone(&domain);
        let writer = tokio::spawn(async move {
            let mut revisions = [0_u64; RESOURCES];
            let mut snapshot_ready_tx = Some(snapshot_ready_tx);
            let mut resume_rx = Some(resume_rx);
            for step in 1..=MUTATIONS {
                let ordinal = step % RESOURCES;
                revisions[ordinal] += 1;
                let revision = revisions[ordinal];
                let id = format!("selection-{ordinal}");
                let deleted = step % 7 == 0;
                {
                    let mut state = writer_domain
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    if deleted {
                        state.remove(&id);
                    } else {
                        state.insert(id.clone(), revision);
                    }
                }
                writer_journal
                    .publish(if deleted {
                        deletion(&id, revision)
                    } else {
                        change(&id, revision)
                    })
                    .unwrap();

                if step == SNAPSHOT_AFTER {
                    snapshot_ready_tx.take().unwrap().send(()).unwrap();
                    resume_rx.take().unwrap().await.unwrap();
                }
                if step % 16 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            writer_journal.checkpoint().0
        });

        snapshot_ready_rx.await.unwrap();
        let (snapshot_cursor, _) = journal.checkpoint();
        resume_tx.send(()).unwrap();
        tokio::task::yield_now().await;
        let snapshot = domain
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let mut subscription = journal.subscribe(Some(&snapshot_cursor)).unwrap();
        let final_cursor = writer.await.unwrap();
        assert_eq!(final_cursor.sequence, MUTATIONS as u64);

        let mut projection = snapshot
            .into_iter()
            .map(|(id, revision)| (id, (revision, true)))
            .collect::<HashMap<_, _>>();
        let mut received = Vec::new();
        let mut previous_sequence = snapshot_cursor.sequence;
        while previous_sequence < final_cursor.sequence {
            let event = subscription.recv().await.unwrap().unwrap().event;
            let sequence = event.cursor.as_ref().unwrap().sequence;
            assert_eq!(sequence, previous_sequence + 1);
            previous_sequence = sequence;
            apply_revisioned_event(&mut projection, &event);
            apply_revisioned_event(&mut projection, &event);
            received.push(event);
        }

        let expected = domain
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        assert_eq!(visible_projection(&projection), expected);

        for event in received.iter().rev() {
            apply_revisioned_event(&mut projection, event);
        }
        assert_eq!(
            visible_projection(&projection),
            expected,
            "duplicates and stale reordered events must not overwrite newer state"
        );
    }

    #[test]
    fn rejects_foreign_and_evicted_cursors_with_replay_gap() {
        let journal = StateJournal::new("instance", config(1, 2));
        journal.publish(change("selection", 1)).unwrap();
        journal.publish(change("selection", 2)).unwrap();

        let old = Cursor {
            server_instance_id: "instance".to_string(),
            sequence: 0,
        };
        let old_error = match journal.subscribe(Some(&old)) {
            Ok(_) => panic!("evicted cursor unexpectedly subscribed"),
            Err(error) => error,
        };
        assert_eq!(old_error.code(), DdbErrorCode::ReplayGap);
        let foreign = Cursor {
            server_instance_id: "other".to_string(),
            sequence: 2,
        };
        let foreign_error = match journal.subscribe(Some(&foreign)) {
            Ok(_) => panic!("foreign cursor unexpectedly subscribed"),
            Err(error) => error,
        };
        assert_eq!(foreign_error.code(), DdbErrorCode::ReplayGap);
        assert!(journal.subscribe(None).is_ok());
    }

    #[tokio::test]
    async fn slow_subscriber_never_blocks_publish_and_observes_a_gap() {
        let journal = StateJournal::new("instance", config(8, 1));
        let mut subscription = journal.subscribe(None).unwrap();
        for revision in 1..=4 {
            journal.publish(change("selection", revision)).unwrap();
        }
        assert_eq!(
            subscription.recv().await.unwrap_err().code(),
            DdbErrorCode::ReplayGap
        );
    }

    #[tokio::test]
    async fn shutdown_wakes_subscribers() {
        let journal = StateJournal::new("instance", config(8, 2));
        let mut subscription = journal.subscribe(None).unwrap();
        journal.shutdown();
        assert!(subscription.recv().await.unwrap().is_none());
    }
}
