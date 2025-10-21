use std::fmt::Debug;

use crate::connection::{
    ssh_client::{SSHConnection, SSHCred},
    RemoteOperatable, SSHIo,
};
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

pub type DbgController = Box<dyn DbgControllable<InputType = bytes::Bytes>>;

#[async_trait]
pub trait DbgControllable: Debug + Sync + Send {
    type InputType;

    async fn start(&mut self, cmd: &str) -> Result<SSHIo>;
    // async fn write(&self, data: Self::InputType) -> Result<()>;
    // async fn read(&mut self) -> Result<Bytes>;
    fn is_open(&self) -> bool;
    async fn close(&mut self) -> Result<()>;
}

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
    ctrl: BaseController<SSHConnection>,
}

impl SSHAttachController {
    pub fn new(cred: SSHCred) -> Self {
        Self {
            ctrl: BaseController::new(SSHConnection::new(cred, None)),
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