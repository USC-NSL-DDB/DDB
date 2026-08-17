use std::{marker::PhantomData, pin::Pin};

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use serde::de::DeserializeOwned;

use crate::{ClientError, Result};

type ByteStream =
    Pin<Box<dyn Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static>>;

/// Incremental bounded decoder for DDB's newline-delimited ProtoJSON streams.
///
/// Empty heartbeat lines are consumed internally and never surfaced as events.
pub struct NdjsonStream<T> {
    inner: ByteStream,
    buffer: BytesMut,
    max_line_bytes: usize,
    finished: bool,
    marker: PhantomData<T>,
}

impl<T> NdjsonStream<T>
where
    T: DeserializeOwned,
{
    pub(crate) fn from_response(response: reqwest::Response, max_line_bytes: usize) -> Self {
        Self {
            inner: Box::pin(response.bytes_stream()),
            buffer: BytesMut::new(),
            max_line_bytes,
            finished: false,
            marker: PhantomData,
        }
    }

    #[cfg(test)]
    fn from_chunks(
        chunks: impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
        max_line_bytes: usize,
    ) -> Self {
        Self {
            inner: Box::pin(chunks),
            buffer: BytesMut::new(),
            max_line_bytes,
            finished: false,
            marker: PhantomData,
        }
    }

    /// Returns the next typed event, skipping heartbeat lines.
    pub async fn next(&mut self) -> Result<Option<T>> {
        loop {
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line = self.buffer.split_to(newline + 1);
                line.truncate(newline);
                if line.last() == Some(&b'\r') {
                    line.truncate(line.len() - 1);
                }
                if line.is_empty() {
                    continue;
                }
                if line.len() > self.max_line_bytes {
                    return Err(ClientError::PayloadTooLarge {
                        limit: self.max_line_bytes,
                    });
                }
                return serde_json::from_slice(&line)
                    .map(Some)
                    .map_err(|error| ClientError::Protocol(error.to_string()));
            }

            if self.buffer.len() > self.max_line_bytes {
                return Err(ClientError::PayloadTooLarge {
                    limit: self.max_line_bytes,
                });
            }
            if self.finished {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let line = self.buffer.split().freeze();
                return serde_json::from_slice(&line)
                    .map(Some)
                    .map_err(|error| ClientError::Protocol(error.to_string()));
            }

            match self.inner.next().await {
                Some(Ok(chunk)) => self.buffer.extend_from_slice(&chunk),
                Some(Err(error)) => return Err(ClientError::Transport(error)),
                None => self.finished = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::stream;
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct Event {
        sequence: u64,
    }

    #[tokio::test]
    async fn decodes_fragmented_lines_and_skips_heartbeats() {
        let chunks = stream::iter([
            Ok(Bytes::from_static(b"{\"se")),
            Ok(Bytes::from_static(b"quence\":1}\n\n{\"sequence\":2}\r\n")),
        ]);
        let mut events = NdjsonStream::<Event>::from_chunks(chunks, 128);

        assert_eq!(events.next().await.unwrap(), Some(Event { sequence: 1 }));
        assert_eq!(events.next().await.unwrap(), Some(Event { sequence: 2 }));
        assert_eq!(events.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_an_unbounded_event_before_allocating_indefinitely() {
        let chunks = stream::iter([Ok(Bytes::from(vec![b'x'; 33]))]);
        let mut events = NdjsonStream::<Event>::from_chunks(chunks, 32);

        assert!(matches!(
            events.next().await,
            Err(ClientError::PayloadTooLarge { limit: 32 })
        ));
    }
}
