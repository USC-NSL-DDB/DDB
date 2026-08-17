use std::{future::Future, time::Duration};

use ddb_api_types::v2;

use crate::{http::proto_duration, ClientError, DdbClient, NdjsonStream, Result};

/// Filter and bounded reconnect policy for [`OutputSync`].
#[derive(Clone, Debug)]
pub struct OutputSyncOptions {
    pub filter: Option<v2::OutputFilter>,
    pub reconnect_initial_delay: Duration,
    pub reconnect_max_delay: Duration,
    /// `None` retries for the lifetime of the consumer. `Some(0)` disables
    /// reconnect attempts after a connection has failed.
    pub max_reconnect_attempts: Option<u32>,
}

impl Default for OutputSyncOptions {
    fn default() -> Self {
        Self {
            filter: None,
            reconnect_initial_delay: Duration::from_millis(100),
            reconnect_max_delay: Duration::from_secs(5),
            max_reconnect_attempts: None,
        }
    }
}

/// One observable output delivery or reconnect transition.
#[derive(Clone, Debug)]
pub enum OutputSyncItem {
    Event(v2::OutputEvent),
    Reconnecting {
        attempt: u32,
        delay: Duration,
        reason: String,
    },
    /// Output replay could not continue from the prior cursor. The next call
    /// subscribes from the live edge; consumers should show the loss explicitly.
    Restarting {
        reason: Option<v2::DdbError>,
    },
}

/// Reconnecting output stream with acknowledgement-based cursor tracking.
///
/// An event is acknowledged when the consumer asks for the next item. A
/// transport failure can therefore replay the most recently returned event;
/// consumers that need deduplication may compare its cursor.
pub struct OutputSync {
    client: DdbClient,
    options: OutputSyncOptions,
    stream: Option<NdjsonStream<v2::OutputEvent>>,
    acknowledged_cursor: Option<v2::Cursor>,
    pending_cursor: Option<v2::Cursor>,
    server_instance_id: Option<String>,
    reconnect_attempt: u32,
    wait_before_connect: Option<Duration>,
}

impl std::fmt::Debug for OutputSync {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputSync")
            .field("options", &self.options)
            .field("acknowledged_cursor", &self.acknowledged_cursor)
            .field("server_instance_id", &self.server_instance_id)
            .field("reconnect_attempt", &self.reconnect_attempt)
            .finish_non_exhaustive()
    }
}

impl OutputSync {
    pub(crate) fn new(client: DdbClient, options: OutputSyncOptions) -> Result<Self> {
        validate_reconnect_policy(options.reconnect_initial_delay, options.reconnect_max_delay)?;
        Ok(Self {
            client,
            options,
            stream: None,
            acknowledged_cursor: None,
            pending_cursor: None,
            server_instance_id: None,
            reconnect_attempt: 0,
            wait_before_connect: None,
        })
    }

    pub fn acknowledged_cursor(&self) -> Option<&v2::Cursor> {
        self.acknowledged_cursor.as_ref()
    }

    /// Drops the current transport while preserving the acknowledged cursor.
    pub fn force_reconnect(&mut self) {
        self.stream = None;
        self.pending_cursor = None;
        self.wait_before_connect = None;
    }

    pub async fn next(&mut self) -> Result<OutputSyncItem> {
        if let Some(cursor) = self.pending_cursor.take() {
            self.acknowledged_cursor = Some(cursor);
        }

        loop {
            if self.stream.is_none() {
                if let Some(delay) = self.wait_before_connect.take() {
                    tokio::time::sleep(delay).await;
                }
                match self.subscribe().await {
                    Ok(stream) => {
                        self.stream = Some(stream);
                        self.reconnect_attempt = 0;
                    }
                    Err(error) => return self.handle_connect_failure(error),
                }
            }

            let item = self
                .stream
                .as_mut()
                .expect("stream was connected above")
                .next()
                .await;
            match item {
                Ok(Some(event)) => {
                    let cursor = event.cursor.clone().ok_or_else(|| {
                        ClientError::Protocol("output event omitted cursor".to_string())
                    })?;
                    if cursor.server_instance_id.is_empty() {
                        return Err(ClientError::Protocol(
                            "output event cursor omitted server_instance_id".to_string(),
                        ));
                    }
                    if let Some(server_instance_id) = self.server_instance_id.as_deref() {
                        if server_instance_id != cursor.server_instance_id {
                            let reason =
                                replay_gap("output stream moved to a different server instance");
                            self.restart_from_live_edge();
                            return Ok(OutputSyncItem::Restarting {
                                reason: Some(reason),
                            });
                        }
                    } else {
                        self.server_instance_id = Some(cursor.server_instance_id.clone());
                    }
                    if self
                        .acknowledged_cursor
                        .as_ref()
                        .is_some_and(|acknowledged| cursor.sequence <= acknowledged.sequence)
                    {
                        continue;
                    }
                    self.pending_cursor = Some(cursor);
                    return Ok(OutputSyncItem::Event(event));
                }
                Ok(None) => {
                    self.stream = None;
                    return self.handle_connect_failure(ClientError::StreamEnded);
                }
                Err(error) if error.requires_rehydration() => {
                    let reason = error.ddb_error().cloned();
                    self.restart_from_live_edge();
                    return Ok(OutputSyncItem::Restarting { reason });
                }
                Err(error) => {
                    self.stream = None;
                    return self.handle_connect_failure(error);
                }
            }
        }
    }

    fn subscribe(
        &self,
    ) -> impl Future<Output = Result<NdjsonStream<v2::OutputEvent>>> + Send + 'static {
        let client = self.client.clone();
        let request = v2::SubscribeOutputRequest {
            context: None,
            after_cursor: self.acknowledged_cursor.clone(),
            filter: self.options.filter.clone(),
        };
        async move { client.subscribe_output(request).await }
    }

    fn handle_connect_failure(&mut self, error: ClientError) -> Result<OutputSyncItem> {
        if error.requires_rehydration() {
            let reason = error.ddb_error().cloned();
            self.restart_from_live_edge();
            return Ok(OutputSyncItem::Restarting { reason });
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
        Ok(OutputSyncItem::Reconnecting {
            attempt,
            delay,
            reason: error.to_string(),
        })
    }

    fn restart_from_live_edge(&mut self) {
        self.stream = None;
        self.acknowledged_cursor = None;
        self.pending_cursor = None;
        self.server_instance_id = None;
        self.reconnect_attempt = 0;
        self.wait_before_connect = None;
    }
}

fn validate_reconnect_policy(initial: Duration, maximum: Duration) -> Result<()> {
    if initial.is_zero() {
        return Err(ClientError::InvalidConfig(
            "reconnect_initial_delay must be greater than zero".to_string(),
        ));
    }
    if maximum < initial {
        return Err(ClientError::InvalidConfig(
            "reconnect_max_delay must not be shorter than reconnect_initial_delay".to_string(),
        ));
    }
    Ok(())
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
    fn output_sync_defaults_are_bounded_and_retry_forever() {
        let options = OutputSyncOptions::default();
        assert_eq!(options.reconnect_initial_delay, Duration::from_millis(100));
        assert_eq!(options.reconnect_max_delay, Duration::from_secs(5));
        assert_eq!(options.max_reconnect_attempts, None);
    }

    #[test]
    fn output_sync_rejects_invalid_backoff() {
        assert!(validate_reconnect_policy(Duration::ZERO, Duration::from_secs(1)).is_err());
        assert!(validate_reconnect_policy(Duration::from_secs(2), Duration::from_secs(1)).is_err());
    }
}
