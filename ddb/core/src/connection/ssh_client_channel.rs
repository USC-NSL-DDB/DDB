use anyhow::Result;
use async_trait::async_trait;
use russh::{
    client::{self, Config, Handle, Handler, Session},
    keys::{key::PrivateKeyWithHashAlg, load_secret_key},
    ChannelId, Disconnect,
};
use std::{fmt, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::watch, time};
use tracing::debug;

use super::{RemoteConnectable, RunningTransport, TransportEvent, TransportRequest};
use crate::common::default_vals::DEFAULT_SSH_PRIVATE_KEY_PATH;

/// Connects and password-authenticates the shared bastion session that proxy
/// SSH transports tunnel through.
pub async fn connect_jump_host(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<Arc<Handle<SSHProxyClientHandler>>> {
    use anyhow::{bail, Context};

    let (exited_sender, _exited) = watch::channel(false);
    let config = Config {
        nodelay: true,
        ..Config::default()
    };
    let mut jump_host = client::connect(
        Arc::new(config),
        (host.to_string(), port),
        SSHProxyClientHandler(exited_sender),
    )
    .await
    .context("failed to connect to the SSH jump host")?;

    match jump_host
        .authenticate_password(user.to_string(), password.to_string())
        .await
        .context("failed to authenticate with the SSH jump host")?
    {
        client::AuthResult::Success => {
            debug!("jump-host password authentication succeeded");
        }
        client::AuthResult::Failure {
            remaining_methods, ..
        } => {
            bail!(
                "jump-host password authentication failed; remaining methods: {:?}",
                remaining_methods
            );
        }
    }

    // OpenSSH enables TCP_NODELAY when a session command starts, but not for
    // connections that only use direct-tcpip channels. Run a no-op command
    // once so small forwarded replies are not held behind the peer's delayed
    // ACK timer.
    let mut latency_channel = jump_host
        .channel_open_session()
        .await
        .context("failed to open the SSH latency warm-up channel")?;
    latency_channel
        .exec(true, "true")
        .await
        .context("failed to prime the SSH jump host")?;
    while latency_channel.wait().await.is_some() {}

    Ok(Arc::new(jump_host))
}
/// SSHProxyCred holds the credentials to connect to the "inner" host
/// (the one behind the bastion). We still need a private key for
/// the second hop's authentication.
#[derive(Debug, Clone)]
pub struct SSHProxyCred {
    pub target_hostname: String,
    pub target_port: u32,
    pub target_username: String,
    pub target_password: Option<String>,
    pub target_private_key_path: Option<PathBuf>,
}

impl SSHProxyCred {
    pub fn new(
        target_hostname: &str,
        target_port: u32,
        target_username: &str,
        target_private_key_path: Option<PathBuf>,
        target_password: Option<String>,
    ) -> Self {
        SSHProxyCred {
            target_hostname: target_hostname.to_string(),
            target_port,
            target_username: target_username.to_string(),
            target_password,
            target_private_key_path: target_private_key_path
                .or_else(|| Some(DEFAULT_SSH_PRIVATE_KEY_PATH.clone())),
        }
    }
}

/// This is almost identical to SSHClientHandler from your SSHConnection.
/// It's used by russh to handle server key checks, exit status, etc.
#[derive(Debug)]
pub struct SSHProxyClientHandler(pub watch::Sender<bool>);

impl Handler for SSHProxyClientHandler {
    type Error = russh::Error;

    #[allow(unused_variables)]
    async fn exit_status(
        &mut self,
        channel: ChannelId,
        exit_status: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!("Exit status (proxy connection): {}", exit_status);
        // indicate the remote program (inner host) exited
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

/// SSHProxyConnection is similar to SSHConnection, but:
/// 1) We hold a handle to an *already-connected* "outer" session (the bastion).
/// 2) Instead of connecting directly, we open a direct TCP/IP channel to the
///    target host and then run the SSH handshake over that channel.
pub struct SSHProxyConnection {
    /// This is the already-connected session (bastion) through which we'll open a channel.
    bastion_session: Arc<Handle<SSHProxyClientHandler>>,
    /// Credentials to connect to the target behind the bastion.
    cred: SSHProxyCred,

    /// The new "inner" SSH session once we have hopped through the bastion.
    inner_session: Option<Handle<SSHProxyClientHandler>>,
    config: Arc<Config>,

    /// Watch channel to determine if the remote process has exited.
    exited: watch::Receiver<bool>,
    exited_sender: watch::Sender<bool>,

    /// Task handle for the background polling of the SSH channel.
    poll_handle: Option<tokio::task::JoinHandle<()>>,
}

impl fmt::Debug for SSHProxyConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SSHProxyConnection")
            .field("cred", &self.cred)
            .field("config", &self.config)
            .finish()
    }
}

impl SSHProxyConnection {
    /// Create a new proxy connection
    pub fn new(
        bastion_session: Arc<Handle<SSHProxyClientHandler>>,
        cred: SSHProxyCred,
        config: Option<Arc<Config>>,
    ) -> Self {
        let (exited_sender, exited) = watch::channel(false);
        SSHProxyConnection {
            bastion_session,
            cred,
            inner_session: None,
            config: config.unwrap_or_else(|| Arc::new(Config::default())),
            exited,
            exited_sender,
            poll_handle: None,
        }
    }

    /// Actually do the "SSH over SSH" connection:
    /// 1) Open a direct TCP/IP channel to the target using the already-connected bastion session.
    /// 2) Run a new SSH client handshake over that channel using `connect_stream`.
    /// 3) Authenticate with the target.
    async fn try_connect(&mut self) -> Result<()> {
        // Step 1: open direct TCP/IP channel through the bastion
        let direct_tcp_chan = self
            .bastion_session
            .channel_open_direct_tcpip(
                self.cred.target_hostname.clone(),
                self.cred.target_port,
                // originator IP and port - typically "127.0.0.1", 0
                // or any IP/port the server allows
                "127.0.0.1".to_string(),
                0,
            )
            .await?;

        // Step 2: convert the direct TCP/IP channel into a "stream"
        let tcp_stream = direct_tcp_chan.into_stream();

        // Step 3: run the new handshake on top of that stream
        let mut session = client::connect_stream(
            self.config.clone(),
            tcp_stream,
            SSHProxyClientHandler(self.exited_sender.clone()),
        )
        .await?;
        // Step 4: authenticate to the target (inner) host
        if let Some(password) = &self.cred.target_password {
            debug!("Attempting password authentication");
            match session
                .authenticate_password(self.cred.target_username.clone(), password)
                .await
            {
                Ok(auth_result) => match auth_result {
                    russh::client::AuthResult::Success => {
                        debug!("Password authentication successful");
                    }
                    russh::client::AuthResult::Failure {
                        remaining_methods, ..
                    } => {
                        return Err(anyhow::anyhow!(
                            "Password authentication failed. Available methods: {:?}",
                            remaining_methods
                        ));
                    }
                },
                Err(e) => {
                    return Err(anyhow::anyhow!("Authentication error: {:?}", e));
                }
            }
        } else {
            debug!("Attempting public key authentication");
            let key_pair =
                load_secret_key(self.cred.target_private_key_path.clone().unwrap(), None)?;
            match session
                .authenticate_publickey(
                    self.cred.target_username.clone(),
                    PrivateKeyWithHashAlg::new(
                        Arc::new(key_pair),
                        session.best_supported_rsa_hash().await.unwrap().flatten(),
                    ),
                )
                .await
            {
                Ok(auth_result) => match auth_result {
                    russh::client::AuthResult::Success => {
                        debug!("Public key authentication successful");
                    }
                    russh::client::AuthResult::Failure {
                        remaining_methods, ..
                    } => {
                        return Err(anyhow::anyhow!(
                            "Public key authentication failed. Available methods: {:?}",
                            remaining_methods
                        ));
                    }
                },
                Err(e) => {
                    return Err(anyhow::anyhow!("Authentication error: {:?}", e));
                }
            }
        }

        self.inner_session = Some(session);
        Ok(())
    }
}

#[async_trait]
impl RemoteConnectable for SSHProxyConnection {
    async fn connect(&mut self) -> Result<()> {
        // Use a simple retry mechanism (just like your original code).
        let mut counter = 0;
        while counter < 5 {
            match self.try_connect().await {
                Ok(_) => {
                    debug!("(Proxy) Connected to target via bastion.");
                    return Ok(());
                }
                Err(e) => {
                    debug!(
                        "(Proxy) Failed to connect via bastion: {}. Retrying... (attempt {})",
                        e,
                        counter + 1
                    );
                }
            }
            time::sleep(Duration::from_millis(500)).await;
            counter += 1;
        }
        Err(anyhow::anyhow!(
            "(Proxy) Failed to connect to target after 5 retries."
        ))
    }

    async fn start(&mut self, cmd: &str) -> Result<RunningTransport> {
        if let Some(s) = &self.inner_session {
            // Open a "session" channel in the inner SSH session
            let chan = s.channel_open_session().await?;
            // Exec the command on the "inner" host
            chan.exec(true, cmd).await?;

            // Create a local channel for sending data to SSH
            let (in_tx, in_rx) = flume::bounded::<TransportRequest>(1024);
            let (out_tx, out_rx) = flume::bounded::<TransportEvent>(1024);

            // Start a background task to poll the SSH channel
            self.poll_handle = Some(tokio::spawn(super::ssh_driver::run(
                chan,
                in_rx,
                out_tx,
                "proxy SSH",
            )));

            return Ok(RunningTransport::new(in_tx, out_rx));
        }
        Err(anyhow::anyhow!(
            "(Proxy) Inner session is not available (not connected)."
        ))
    }

    async fn disconnect(&mut self) -> Result<()> {
        let result = if let Some(s) = self.inner_session.take() {
            s.disconnect(Disconnect::ByApplication, "Exit from DCore (proxy).", "en")
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
        // If the remote program has exited (watch channel is true), then we consider ourselves disconnected.
        if *self.exited.borrow() {
            return false;
        }

        // We also might want to check if the underlying session is still alive, but that can be
        // trickier to do reliably with russh. For demonstration, we rely on the watch.
        true
    }
}
