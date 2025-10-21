use bytes::Bytes;
use anyhow::Result;
use async_trait::async_trait;

pub mod ssh_client;
pub mod ssh_client_channel;

pub struct SSHIo {
    /// For sending data _to_ remote
    pub in_tx: flume::Sender<Bytes>,
    /// For receiving data _from_ remote
    pub out_rx: flume::Receiver<Bytes>,
}
#[async_trait]
pub trait RemoteConnectable {
    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    fn is_connected(&self) -> bool;
    async fn start(&mut self, cmd: &str) -> Result<SSHIo>;
}

// #[async_trait]
// pub trait RemoteWritable: RemoteConnectable {
//     async fn write(&self, data: Bytes) -> Result<()>;
// }

// #[async_trait]
// pub trait RemoteReadable: RemoteConnectable {
//     async fn read(&mut self) -> Result<Bytes>;
// }

// pub trait RemoteOperatable: RemoteWritable + RemoteReadable + Sync + Send {}

// impl<T> RemoteOperatable for T where T: RemoteWritable + RemoteReadable + Sync + Send {}

// FIXME: legacy traits when the write/read APIs are required.
// Since now we stick with channeling, no such APIs are required
// Later, we can remove RemoteOperatable trait and leave only RemoteConnectable
// or, use RemoteOperatable to replace RemoteConnectable.
pub trait RemoteOperatable: RemoteConnectable + Sync + Send {}

impl<T> RemoteOperatable for T where T: RemoteConnectable + Sync + Send {}
