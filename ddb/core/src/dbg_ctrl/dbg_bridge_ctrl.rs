use std::{fmt::Debug, sync::Arc};

use crate::connection::{ssh_client_channel::SSHProxyConnection, RemoteOperatable, SSHIo};
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use russh::client::Handle;

use super::DbgControllable;

#[derive(Debug)]
pub struct BaseController<T>
where
    T: RemoteOperatable,
{
    client: T,
}

impl<T> BaseController<T>
where
    T: RemoteOperatable,
{
    pub fn new(client: T) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<T> DbgControllable for BaseController<T>
where
    T: RemoteOperatable + Debug,
{
    type InputType = Bytes;

    async fn start(&mut self, cmd: &str) -> Result<SSHIo> {
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

#[derive(Debug)]
pub struct SSHAttachController {
    ctrl: BaseController<SSHProxyConnection>,
}

impl SSHAttachController {
    pub fn new(
        cred: crate::connection::ssh_client_channel::SSHProxyCred,
        tunnel: Arc<Handle<crate::connection::ssh_client_channel::SSHProxyClientHandler>>,
    ) -> Self {
        Self {
            ctrl: BaseController::new(SSHProxyConnection::new(tunnel, cred, None)),
        }
    }
}

#[async_trait]
impl DbgControllable for SSHAttachController {
    type InputType = Bytes;

    async fn start(&mut self, cmd: &str) -> Result<SSHIo> {
        // let cmd = format!("gdb -p {}", self.pid);
        // TODO: move cmd to this function instead of an argument
        self.ctrl.start(&cmd).await
    }

    fn is_open(&self) -> bool {
        self.ctrl.is_open()
    }

    async fn close(&mut self) -> Result<()> {
        self.ctrl.close().await
    }
}
