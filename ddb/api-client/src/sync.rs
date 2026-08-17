use std::{future::Future, time::Duration};

use ddb_api_types::v2;

use crate::{
    http::proto_duration, ClientError, DdbClient, NdjsonStream, ProjectionUpdate, Result,
    StateProjection,
};

/// Snapshot selection and bounded reconnect policy for [`StateSync`].
#[derive(Clone, Debug)]
pub struct StateSyncOptions {
    pub sections: Vec<i32>,
    pub target: Option<v2::Target>,
    pub filter: Option<v2::StateEventFilter>,
    pub reconnect_initial_delay: Duration,
    pub reconnect_max_delay: Duration,
    /// `None` retries for the lifetime of the consumer. `Some(0)` disables
    /// reconnect attempts after a connection has failed.
    pub max_reconnect_attempts: Option<u32>,
}

impl Default for StateSyncOptions {
    fn default() -> Self {
        Self {
            sections: Vec::new(),
            target: None,
            filter: None,
            reconnect_initial_delay: Duration::from_millis(100),
            reconnect_max_delay: Duration::from_secs(5),
            max_reconnect_attempts: None,
        }
    }
}

/// One observable step in snapshot hydration and replayable state delivery.
#[derive(Clone, Debug)]
pub enum StateSyncItem {
    /// A complete replacement for the consumer's derived projection.
    Snapshot(v2::Snapshot),
    /// A replayed or live event newer than the last acknowledged cursor.
    Event(v2::StateEvent),
    /// A transient failure was encountered. Calling `next` waits for `delay`
    /// and continues with the last acknowledged cursor.
    Reconnecting {
        attempt: u32,
        delay: Duration,
        reason: String,
    },
    /// The previous projection and cursor are no longer valid. Consumers must
    /// clear derived state; a subsequent item will be a fresh snapshot.
    Rehydrating { reason: Option<v2::DdbError> },
}

/// State synchronization after snapshot/event convergence has been applied by
/// the SDK's revision-aware projection.
#[derive(Clone, Debug)]
pub enum ProjectedStateSyncItem {
    Snapshot,
    Event(Box<v2::StateEvent>),
    Reconnecting {
        attempt: u32,
        delay: Duration,
        reason: String,
    },
    Rehydrating {
        reason: Option<Box<v2::DdbError>>,
    },
}

/// High-level state workflow for frontends. It owns both reconnection and the
/// idempotent public resource projection.
pub struct ProjectedStateSync {
    sync: StateSync,
    projection: Option<StateProjection>,
}

impl std::fmt::Debug for ProjectedStateSync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectedStateSync")
            .field("sync", &self.sync)
            .field("has_projection", &self.projection.is_some())
            .finish()
    }
}

impl ProjectedStateSync {
    pub(crate) fn new(client: DdbClient, options: StateSyncOptions) -> Result<Self> {
        Ok(Self {
            sync: StateSync::new(client, options)?,
            projection: None,
        })
    }

    pub fn projection(&self) -> Option<&StateProjection> {
        self.projection.as_ref()
    }

    pub fn current_snapshot(&self) -> Option<v2::Snapshot> {
        self.projection.as_ref().map(StateProjection::snapshot)
    }

    pub fn force_reconnect(&mut self) {
        self.sync.force_reconnect();
    }

    pub async fn next(&mut self) -> Result<ProjectedStateSyncItem> {
        loop {
            match self.sync.next().await? {
                StateSyncItem::Snapshot(snapshot) => {
                    self.projection = Some(StateProjection::from_snapshot(snapshot)?);
                    return Ok(ProjectedStateSyncItem::Snapshot);
                }
                StateSyncItem::Event(event) => {
                    let projection = self.projection.as_mut().ok_or_else(|| {
                        ClientError::Protocol(
                            "state sync delivered an event before a snapshot".to_string(),
                        )
                    })?;
                    match projection.apply_event(event.clone())? {
                        ProjectionUpdate::Applied => {
                            return Ok(ProjectedStateSyncItem::Event(Box::new(event)));
                        }
                        ProjectionUpdate::IgnoredStale => continue,
                        ProjectionUpdate::RehydrationRequired(reason) => {
                            self.projection = None;
                            self.sync.require_hydration();
                            return Ok(ProjectedStateSyncItem::Rehydrating { reason });
                        }
                    }
                }
                StateSyncItem::Reconnecting {
                    attempt,
                    delay,
                    reason,
                } => {
                    return Ok(ProjectedStateSyncItem::Reconnecting {
                        attempt,
                        delay,
                        reason,
                    });
                }
                StateSyncItem::Rehydrating { reason } => {
                    self.projection = None;
                    return Ok(ProjectedStateSyncItem::Rehydrating {
                        reason: reason.map(Box::new),
                    });
                }
            }
        }
    }
}

/// Reconnecting snapshot-plus-event workflow used by frontend clients.
///
/// An event is acknowledged when the consumer asks for the next item. This
/// means a disconnect replays the last returned event unless the consumer had
/// already advanced; applying resource revisions idempotently makes that safe.
pub struct StateSync {
    client: DdbClient,
    options: StateSyncOptions,
    stream: Option<NdjsonStream<v2::StateEvent>>,
    acknowledged_cursor: Option<v2::Cursor>,
    pending_cursor: Option<v2::Cursor>,
    server_instance_id: Option<String>,
    needs_hydration: bool,
    reconnect_attempt: u32,
    wait_before_connect: Option<Duration>,
}

impl std::fmt::Debug for StateSync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateSync")
            .field("options", &self.options)
            .field("acknowledged_cursor", &self.acknowledged_cursor)
            .field("server_instance_id", &self.server_instance_id)
            .field("needs_hydration", &self.needs_hydration)
            .field("reconnect_attempt", &self.reconnect_attempt)
            .finish_non_exhaustive()
    }
}

impl StateSync {
    pub(crate) fn new(client: DdbClient, options: StateSyncOptions) -> Result<Self> {
        if options.reconnect_initial_delay.is_zero() {
            return Err(ClientError::InvalidConfig(
                "reconnect_initial_delay must be greater than zero".to_string(),
            ));
        }
        if options.reconnect_max_delay < options.reconnect_initial_delay {
            return Err(ClientError::InvalidConfig(
                "reconnect_max_delay must not be shorter than reconnect_initial_delay".to_string(),
            ));
        }
        Ok(Self {
            client,
            options,
            stream: None,
            acknowledged_cursor: None,
            pending_cursor: None,
            server_instance_id: None,
            needs_hydration: true,
            reconnect_attempt: 0,
            wait_before_connect: None,
        })
    }

    /// Cursor of the last event acknowledged by requesting another item.
    pub fn acknowledged_cursor(&self) -> Option<&v2::Cursor> {
        self.acknowledged_cursor.as_ref()
    }

    /// Drops the current transport connection while preserving the last
    /// acknowledged cursor. The next call to [`Self::next`] reconnects and
    /// requests replay after that cursor.
    pub fn force_reconnect(&mut self) {
        self.stream = None;
        self.pending_cursor = None;
        self.wait_before_connect = None;
    }

    /// Returns the next snapshot, event, or reconnect diagnostic.
    pub async fn next(&mut self) -> Result<StateSyncItem> {
        if let Some(cursor) = self.pending_cursor.take() {
            self.acknowledged_cursor = Some(cursor);
        }

        loop {
            if self.stream.is_none() {
                if let Some(delay) = self.wait_before_connect.take() {
                    tokio::time::sleep(delay).await;
                }
                match self.connect().await {
                    Ok(Some(snapshot)) => return Ok(StateSyncItem::Snapshot(snapshot)),
                    Ok(None) => continue,
                    Err(error) => return self.handle_connect_failure(error),
                }
            }

            let item = self
                .stream
                .as_mut()
                .expect("stream was checked above")
                .next()
                .await;
            match item {
                Ok(Some(event)) => {
                    let Some(cursor) = self.validate_event_cursor(&event)? else {
                        let reason = Some(replay_gap(
                            "state event belongs to a different server instance",
                        ));
                        self.require_hydration();
                        return Ok(StateSyncItem::Rehydrating { reason });
                    };
                    if self
                        .acknowledged_cursor
                        .as_ref()
                        .is_some_and(|acknowledged| cursor.sequence <= acknowledged.sequence)
                    {
                        continue;
                    }
                    if let Some(v2::state_event::Payload::RequiredResync(resync)) =
                        event.payload.as_ref()
                    {
                        let reason = resync.reason.clone();
                        self.require_hydration();
                        return Ok(StateSyncItem::Rehydrating { reason });
                    }
                    self.pending_cursor = Some(cursor);
                    return Ok(StateSyncItem::Event(event));
                }
                Ok(None) => {
                    self.stream = None;
                    return self.handle_connect_failure(ClientError::StreamEnded);
                }
                Err(error) if error.requires_rehydration() => {
                    let reason = error.ddb_error().cloned();
                    self.require_hydration();
                    return Ok(StateSyncItem::Rehydrating { reason });
                }
                Err(error) => {
                    self.stream = None;
                    return self.handle_connect_failure(error);
                }
            }
        }
    }

    async fn connect(&mut self) -> Result<Option<v2::Snapshot>> {
        if self.needs_hydration {
            let (info, capabilities) = self.client.handshake().await?;
            if info.server_instance_id != capabilities.server_instance_id {
                return Err(ClientError::Protocol(
                    "server info and capabilities identify different server instances".to_string(),
                ));
            }
            let snapshot = self
                .client
                .get_snapshot(v2::GetSnapshotRequest {
                    context: None,
                    sections: self.options.sections.clone(),
                    target: self.options.target.clone(),
                })
                .await?
                .snapshot
                .ok_or_else(|| ClientError::Protocol("GetSnapshot omitted snapshot".to_string()))?;
            let cursor = snapshot.state_event_cursor.clone().ok_or_else(|| {
                ClientError::Protocol("snapshot omitted state_event_cursor".to_string())
            })?;
            if snapshot.server_instance_id.is_empty()
                || snapshot.server_instance_id != info.server_instance_id
                || cursor.server_instance_id != snapshot.server_instance_id
            {
                return Err(ClientError::Protocol(
                    "snapshot and discovery server-instance identities disagree".to_string(),
                ));
            }
            let stream = self.subscribe(Some(cursor.clone())).await?;
            self.stream = Some(stream);
            self.acknowledged_cursor = Some(cursor);
            self.pending_cursor = None;
            self.server_instance_id = Some(snapshot.server_instance_id.clone());
            self.needs_hydration = false;
            self.reconnect_attempt = 0;
            return Ok(Some(snapshot));
        }

        self.stream = Some(self.subscribe(self.acknowledged_cursor.clone()).await?);
        self.reconnect_attempt = 0;
        Ok(None)
    }

    fn subscribe(
        &self,
        after_cursor: Option<v2::Cursor>,
    ) -> impl Future<Output = Result<NdjsonStream<v2::StateEvent>>> + Send + 'static {
        let client = self.client.clone();
        let request = v2::SubscribeStateEventsRequest {
            context: None,
            after_cursor,
            filter: self.options.filter.clone(),
        };
        async move { client.subscribe_state_events(request).await }
    }

    fn validate_event_cursor(&self, event: &v2::StateEvent) -> Result<Option<v2::Cursor>> {
        let cursor = event
            .cursor
            .clone()
            .ok_or_else(|| ClientError::Protocol("state event omitted cursor".to_string()))?;
        if cursor.server_instance_id.is_empty() {
            return Err(ClientError::Protocol(
                "state event cursor omitted server_instance_id".to_string(),
            ));
        }
        if self.server_instance_id.as_deref() != Some(cursor.server_instance_id.as_str()) {
            return Ok(None);
        }
        Ok(Some(cursor))
    }

    fn handle_connect_failure(&mut self, error: ClientError) -> Result<StateSyncItem> {
        if error.requires_rehydration() {
            let reason = error.ddb_error().cloned();
            self.require_hydration();
            return Ok(StateSyncItem::Rehydrating { reason });
        }
        if !error.is_retryable() {
            return Err(error);
        }

        let attempt = self.reconnect_attempt.saturating_add(1);
        if self
            .options
            .max_reconnect_attempts
            .is_some_and(|maximum| attempt > maximum)
        {
            return Err(ClientError::ReconnectExhausted {
                attempts: self.reconnect_attempt,
                last_error: Box::new(error),
            });
        }
        self.reconnect_attempt = attempt;
        let exponential = self
            .options
            .reconnect_initial_delay
            .saturating_mul(1_u32 << attempt.saturating_sub(1).min(16))
            .min(self.options.reconnect_max_delay);
        let delay = error
            .ddb_error()
            .and_then(|error| error.retry_after.as_ref())
            .and_then(proto_duration)
            .map_or(exponential, |suggested| exponential.max(suggested));
        self.wait_before_connect = Some(delay);
        Ok(StateSyncItem::Reconnecting {
            attempt,
            delay,
            reason: error.to_string(),
        })
    }

    fn require_hydration(&mut self) {
        self.stream = None;
        self.acknowledged_cursor = None;
        self.pending_cursor = None;
        self.server_instance_id = None;
        self.needs_hydration = true;
        self.reconnect_attempt = 0;
        self.wait_before_connect = None;
    }
}

fn replay_gap(message: &str) -> v2::DdbError {
    v2::DdbError {
        code: v2::DdbErrorCode::ReplayGap as i32,
        message: message.to_string(),
        retryable: false,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_keep_retrying() {
        let options = StateSyncOptions::default();
        assert!(options.reconnect_initial_delay > Duration::ZERO);
        assert!(options.reconnect_max_delay >= options.reconnect_initial_delay);
        assert_eq!(options.max_reconnect_attempts, None);
    }

    #[test]
    fn invalid_backoff_configuration_is_rejected() {
        let client = DdbClient::new(crate::ClientConfig::new("http://127.0.0.1:1")).unwrap();
        let options = StateSyncOptions {
            reconnect_initial_delay: Duration::from_secs(2),
            reconnect_max_delay: Duration::from_secs(1),
            ..Default::default()
        };
        assert!(matches!(
            client.state_sync(options),
            Err(ClientError::InvalidConfig(_))
        ));
    }
}
