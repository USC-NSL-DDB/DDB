use std::fmt::Debug;

use crate::connection::{RemoteConnectable, RunningTransport};
use anyhow::Result;
use async_trait::async_trait;

pub type DebuggerTransportHandle = Box<dyn DebuggerTransport>;

#[async_trait]
pub trait DebuggerTransport: Debug + Sync + Send {
    async fn launch(&mut self, cmd: &str) -> Result<RunningTransport>;
    fn is_open(&self) -> bool;
    async fn close(&mut self) -> Result<()>;
}

#[derive(Debug)]
pub struct RemoteTransport<T>
where
    T: RemoteConnectable,
{
    client: T,
}

impl<T> RemoteTransport<T>
where
    T: RemoteConnectable,
{
    pub fn new(client: T) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<T> DebuggerTransport for RemoteTransport<T>
where
    T: RemoteConnectable + Debug,
{
    async fn launch(&mut self, cmd: &str) -> Result<RunningTransport> {
        self.client.connect().await?;
        self.client.start(cmd).await
    }

    fn is_open(&self) -> bool {
        self.client.is_connected()
    }

    async fn close(&mut self) -> Result<()> {
        self.client.disconnect().await
    }
}
