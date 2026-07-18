use anyhow::Result;
use async_trait::async_trait;
use russh::{
    client::{self, Config, Handle, Handler, Session},
    keys::{key::PrivateKeyWithHashAlg, load_secret_key},
    ChannelId, Disconnect,
};
use std::{fmt, path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::{sync::watch, time};
use tracing::debug;

use super::{RemoteConnectable, RunningTransport, TransportEvent, TransportRequest};
use crate::common::default_vals::DEFAULT_SSH_PRIVATE_KEY_PATH;

#[derive(Debug, Error)]
pub enum SSHConnectionError {
    /// Retryable errors (network issues, temporary failures)
    #[error(transparent)]
    Retryable(anyhow::Error),
    /// Non-retryable errors (authentication failures)
    #[error(transparent)]
    NonRetryable(anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct SSHCred {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub private_key_path: PathBuf,
}

impl SSHCred {
    pub fn new(
        hostname: &str,
        port: u16,
        username: &str,
        private_key_path: Option<PathBuf>,
    ) -> Self {
        SSHCred {
            hostname: hostname.to_string(),
            port,
            username: username.to_string(),
            private_key_path: private_key_path
                .unwrap_or_else(|| DEFAULT_SSH_PRIVATE_KEY_PATH.clone()),
        }
    }
}

#[derive(Debug)]
pub struct SSHClientHandler(pub watch::Sender<bool>);

impl Handler for SSHClientHandler {
    type Error = russh::Error;

    #[allow(unused_variables)]
    async fn exit_status(
        &mut self,
        channel: ChannelId,
        exit_status: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!("Exit status: {}", exit_status);
        // indicate the remote program exited.
        self.0.send(true).unwrap();
        Ok(())
    }

    #[allow(unused_variables)]
    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        // TODO: properly handle public key checking
        Ok(true)
    }
}

pub struct SSHConnection {
    cred: SSHCred,
    session: Option<Handle<SSHClientHandler>>,
    config: Arc<Config>,

    exited: watch::Receiver<bool>,
    exited_sender: watch::Sender<bool>,
    poll_handle: Option<tokio::task::JoinHandle<()>>,
}

impl fmt::Debug for SSHConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SSHConnection")
            .field("cred", &self.cred)
            .field("config", &self.config)
            .finish()
    }
}

impl SSHConnection {
    pub fn new(cred: SSHCred, config: Option<Arc<Config>>) -> Self {
        let (exited_sender, exited) = watch::channel(false);
        SSHConnection {
            cred,
            session: None,
            config: config.unwrap_or(Arc::new(Config::default())),
            exited,
            exited_sender,
            poll_handle: None,
        }
    }
}

impl SSHConnection {
    async fn try_connect(&mut self) -> Result<(), SSHConnectionError> {
        // TODO: use more sophisticated authentication handling...
        let mut session = client::connect(
            self.config.clone(),
            (self.cred.hostname.clone(), self.cred.port),
            SSHClientHandler(self.exited_sender.clone()),
        )
        .await
        .map_err(|e| {
            SSHConnectionError::Retryable(anyhow::Error::new(e).context(format!(
                "Failed to connect to {}:{} with user {}.",
                self.cred.hostname, self.cred.port, self.cred.username
            )))
        })?;

        let key_pair = load_secret_key(self.cred.private_key_path.clone(), None)
            .map_err(|e| SSHConnectionError::NonRetryable(e.into()))?;

        let auth_result = session
            .authenticate_publickey(
                self.cred.username.clone(),
                PrivateKeyWithHashAlg::new(
                    Arc::new(key_pair),
                    session.best_supported_rsa_hash().await.unwrap().flatten(),
                ),
            )
            .await
            .map_err(|e| {
                SSHConnectionError::NonRetryable(anyhow::Error::new(e).context(format!(
                    "Failed to authenticate with public key: {}",
                    self.cred.private_key_path.display()
                )))
            })?;
        match auth_result {
            russh::client::AuthResult::Success => {
                debug!("SSH authentication accepted.");
            }
            _ => {
                return Err(SSHConnectionError::NonRetryable(anyhow::anyhow!(
                    "SSH authentication failed. Auth result: {:?}",
                    auth_result
                )));
            }
        }
        self.session = Some(session);
        Ok(())
    }
}

#[async_trait]
impl RemoteConnectable for SSHConnection {
    async fn connect(&mut self) -> Result<()> {
        let mut counter = 0;
        loop {
            if counter > 5 {
                return Err(anyhow::anyhow!("Failed to connect after 5 retries."));
            }
            match self.try_connect().await {
                Ok(_) => break,
                Err(SSHConnectionError::NonRetryable(e)) => {
                    return Err(e);
                }
                Err(SSHConnectionError::Retryable(e)) => {
                    debug!("Failed to connect. Err: {}. Retrying...", e);
                }
            }
            time::sleep(Duration::from_millis(500)).await;
            counter += 1;
        }
        Ok(())
    }

    async fn start(&mut self, cmd: &str) -> Result<RunningTransport> {
        if let Some(s) = &self.session {
            let chan = s.channel_open_session().await?;
            chan.exec(true, cmd).await?;

            // Create channel for sending data to SSH
            // This is a workaround for the issue that the SSH library doesn't provide a way to
            // send data to the remote program in a thread-safe concurrent manner.
            let (in_tx, in_rx) = flume::bounded::<TransportRequest>(1024);
            let (out_tx, out_rx) = flume::bounded::<TransportEvent>(1024);

            self.poll_handle = Some(tokio::spawn(super::ssh_driver::run(
                chan,
                in_rx,
                out_tx,
                "direct SSH",
            )));

            return Ok(RunningTransport::new(in_tx, out_rx));
        }
        Err(anyhow::anyhow!("Session is not available."))
    }

    async fn disconnect(&mut self) -> Result<()> {
        let result = if let Some(s) = self.session.take() {
            s.disconnect(Disconnect::ByApplication, "Exit from DCore.", "en")
                .await
                .map_err(anyhow::Error::from)
        } else {
            Ok(())
        };
        if let Some(h) = self.poll_handle.take() {
            h.abort();
        }
        result
    }

    #[inline]
    fn is_connected(&self) -> bool {
        // Note: this is not a perfect check, but it should be good enough for now.
        // Session keeps connected even if the remote program is closed.
        // Therefore, we need to check the exited flag.
        !*self.exited.borrow()
    }
}
