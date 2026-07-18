use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use std::fmt::Debug;

pub mod ssh_client;
pub mod ssh_client_channel;
pub(crate) mod ssh_driver;

/// Events produced by a running debugger transport.
///
/// Keeping stderr and lifecycle changes distinct prevents protocol parsers
/// from treating diagnostics as GDB/MI and gives the session runtime one
/// uniform lifecycle model for local, SSH, proxy, and mock transports.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    Stdout(Bytes),
    Stderr(Bytes),
    Exited(Option<u32>),
    Fault(String),
}

pub(crate) enum TransportRequest {
    Write {
        data: Bytes,
        written: tokio::sync::oneshot::Sender<Result<()>>,
    },
}

#[derive(Clone)]
pub struct TransportWriter {
    requests: flume::Sender<TransportRequest>,
}

impl TransportWriter {
    pub(crate) async fn start_write(
        &self,
        data: Bytes,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<()>>> {
        let (written, result) = tokio::sync::oneshot::channel();
        self.requests
            .send_async(TransportRequest::Write { data, written })
            .await
            .map_err(|_| anyhow::anyhow!("transport request queue is closed"))?;
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) async fn write(&self, data: Bytes) -> Result<()> {
        self.start_write(data)
            .await?
            .await
            .map_err(|_| anyhow::anyhow!("transport stopped before confirming write"))?
    }
}

/// Private channel plumbing for one running transport.
pub struct RunningTransport {
    writer: TransportWriter,
    events: flume::Receiver<TransportEvent>,
}

impl RunningTransport {
    pub(crate) fn new(
        requests: flume::Sender<TransportRequest>,
        events: flume::Receiver<TransportEvent>,
    ) -> Self {
        Self {
            writer: TransportWriter { requests },
            events,
        }
    }

    pub(crate) fn into_parts(self) -> (TransportWriter, flume::Receiver<TransportEvent>) {
        (self.writer, self.events)
    }
}
#[async_trait]
pub trait RemoteConnectable: Debug + Sync + Send {
    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    fn is_connected(&self) -> bool;
    async fn start(&mut self, cmd: &str) -> Result<RunningTransport>;
}
