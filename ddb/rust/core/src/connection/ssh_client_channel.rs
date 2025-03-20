use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use russh::{
    client::{self, Config, Handle, Handler, Msg, Session},
    keys::{key::PrivateKeyWithHashAlg, load_secret_key},
    Channel, ChannelId, ChannelMsg, Disconnect,
};
use std::{fmt, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::watch, time};
use tracing::debug;

use crate::{
    common::default_vals::DEFAULT_SSH_PRIVATE_KEY_PATH,
    dbg_ctrl::{InputReceiver, OutputSender},
};
use super::{RemoteConnectable, SSHIo};
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
            target_password: target_password,
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
            match session.authenticate_password(self.cred.target_username.clone(), password).await {
                Ok(auth_result) => {
                    match auth_result {
                        russh::client::AuthResult::Success => {
                            debug!("Password authentication successful");
                        }
                        russh::client::AuthResult::Failure { remaining_methods } => {
                            return Err(anyhow::anyhow!(
                                "Password authentication failed. Available methods: {:?}", 
                                remaining_methods
                            ));
                        }
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Authentication error: {:?}", 
                        e
                    ));
                }
            }
        } else {
            debug!("Attempting public key authentication");
            let key_pair = load_secret_key(self.cred.target_private_key_path.clone().unwrap(), None)?;
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
                Ok(auth_result) => {
                    match auth_result {
                        russh::client::AuthResult::Success => {
                            debug!("Public key authentication successful");
                        }
                        russh::client::AuthResult::Failure { remaining_methods } => {
                            return Err(anyhow::anyhow!(
                                "Public key authentication failed. Available methods: {:?}", 
                                remaining_methods
                            ));
                        }
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Authentication error: {:?}", 
                        e
                    ));
                }
            }
        }
    
        self.inner_session = Some(session);
        Ok(())
    }

    /// This is identical to the polling logic in your original SSHConnection:
    /// 1) read data from `in_rx` and send it over the SSH channel
    /// 2) read data from the remote side and forward it to `out_tx`
    async fn poll(mut ssh_chan: Channel<Msg>, in_rx: InputReceiver, out_tx: OutputSender) {
        loop {
            tokio::select! {
                // Input from local -> remote
                Ok(data) = in_rx.recv_async() => {
                    debug!("(Proxy) Sending data: {:?}", data);
                    match ssh_chan.data(data.as_ref()).await {
                        Ok(_) => {}
                        Err(e) => {
                            debug!("(Proxy) Error sending input data: {}", e);
                            // Typically break or keep going depending on your logic
                            break;
                        }
                    }
                }

                // Output from remote -> local
                Some(msg) = ssh_chan.wait() => {
                    match msg {
                        ChannelMsg::Data { ref data } => {
                            debug!("(Proxy) Read data: {:?}", std::str::from_utf8(data));
                            if let Err(e) = out_tx.send_async(Bytes::from(data.to_vec())).await {
                                debug!("(Proxy) Failed to send output data: {}", e);
                                break;
                            }
                        }
                        ChannelMsg::Eof => {
                            debug!("(Proxy) EOF received");
                            break;
                        }
                        ChannelMsg::ExitStatus { .. } => {
                            debug!("(Proxy) Exit status received");
                            break;
                        }
                        other => {
                            debug!("(Proxy) Ignoring other SSH message: {:?}", other);
                        }
                    }
                }

                else => {
                    // If channels close, etc.
                    break;
                }
            }
        }
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
                        e, counter + 1
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

    async fn start(&mut self, cmd: &str) -> Result<SSHIo> {
        if let Some(s) = &self.inner_session {
            // Open a "session" channel in the inner SSH session
            let chan = s.channel_open_session().await?;
            // Exec the command on the "inner" host
            chan.exec(true, cmd).await?;

            // Create a local channel for sending data to SSH
            let (in_tx, in_rx) = flume::bounded::<Bytes>(1024);
            let (out_tx, out_rx) = flume::bounded::<Bytes>(1024);

            // Start a background task to poll the SSH channel
            self.poll_handle = Some(tokio::spawn(Self::poll(chan, in_rx, out_tx)));

            return Ok(SSHIo { in_tx, out_rx });
        }
        Err(anyhow::anyhow!(
            "(Proxy) Inner session is not available (not connected)."
        ))
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(s) = self.inner_session.take() {
            s.disconnect(Disconnect::ByApplication, "Exit from DCore (proxy).", "en")
                .await?;
        }
        if let Some(h) = self.poll_handle.take() {
            h.abort();
        }
        Ok(())
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



// Typically in tests/integration_tests/ssh_proxy_integration.rs
#[cfg(test)]
mod integration_tests {

    use super::*;
    use std::sync::Once;
    use tracing::debug;
    use tracing_subscriber::{FmtSubscriber, EnvFilter};

    static INIT: Once = Once::new();

    fn init_tracing() {
        INIT.call_once(|| {
            // Build a subscriber that reads filter directives from the `RUST_LOG`
            // env var if set, or defaults to "debug" otherwise:
            let subscriber = FmtSubscriber::builder()
                .with_env_filter(EnvFilter::from_default_env())
                .with_test_writer() // Use a writer suitable for tests
                .with_target(false) // If you prefer to hide module/target info
                .finish();

            tracing::subscriber::set_global_default(subscriber)
                .expect("Failed to set tracing subscriber");
        });
    }
    use super::*;
    use anyhow::Result;
    use std::sync::Arc;
    use tokio::{runtime::Runtime, time::Duration};
    use russh::client::Config as SSHConfig;

    /// This test assumes:
    /// 1) You have a local or Docker-based "bastion" SSH server accessible at some address/port.
    /// 2) That bastion can open a direct TCP/IP channel to the real "target" SSH server.
    /// 3) You have valid private keys for both steps.
    ///
    /// For instance, you might run two containers:
    /// - Bastion container on port 2222
    /// - Target container only accessible from the Bastion
    ///
    /// Then you supply key paths and credentials that exist on your test machine.
    #[tokio::test]
    async fn test_ssh_proxy_connection_end_to_end() -> Result<()> {
        init_tracing();
        // 1. Connect to the bastion.
        let bastion_config = Arc::new(SSHConfig::default());
        let (bastion_exited_tx, bastion_exited_rx) = tokio::sync::watch::channel(true);

        // Step 1a. Real connect to the bastion (replace placeholders)
        let mut bastion_handle = russh::client::connect(
            bastion_config.clone(),
            ("10.43.142.19", 22),
            SSHProxyClientHandler(bastion_exited_tx.clone()),
        ).await?;

        // Step 1b. Authenticate on the bastion:
        // let bastion_key = load_secret_key("/path/to/bastion.key", None)?;
        bastion_handle.authenticate_password("root", "password123").await?;
        println!("Bastion connected");
        // Try a simple command on bastion to verify connectivity
        let mut channel = bastion_handle.channel_open_session().await?;
        channel.exec(true, "echo 'Bastion connection test'").await?;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { ref data }) => {
                    println!("Bastion output: {}", std::str::from_utf8(data).unwrap());
                }
                Some(ChannelMsg::Eof) => {
                    println!("Received EOF");
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    println!("Command exited with status: {}", exit_status);
                    assert_eq!(exit_status, 0, "Command failed");
                    break;
                }
                other => {
                    println!("Other message: {:?}", other);
                }
            }
        }
        println!("Executed command on bastion");
        
        // Wait briefly for command execution
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        // Check channel exit status
        if let Some(ChannelMsg::ExitStatus { exit_status }) = channel.wait().await {
            assert_eq!(exit_status, 0, "Bastion test command failed with exit status {}", exit_status);
        }
        
        // Close test channel
        channel.close().await?;

        // 2. Create the SSHProxyConnection to the "inner" target using the bastion session
        let target_cred = SSHProxyCred::new(
            "10.42.0.27",  // A hostname resolvable only by the bastion, for instance
            22,
            "root",
            Some(PathBuf::from("/path/to/target.key")),
            Some("admin123".to_string()),
        );
        println!("Target cred: {:?}", target_cred); 
        let mut proxy_conn = SSHProxyConnection::new(
            Arc::new(bastion_handle), 
            target_cred,
            Some(bastion_config.clone()),
        );

        // 3. connect() to the target behind the bastion
        proxy_conn.connect().await?;
        println!("Proxy conn: {:?}", proxy_conn);
        // assert!(proxy_conn.is_connected(), "Should be connected after successful hop.");

        // 4. start() a command on the target
        let ssh_io = proxy_conn.start("gdb").await?;

        // Wait briefly so the remote side can output
        tokio::time::sleep(Duration::from_secs(1)).await;

        // 5. Read output from the target
        // Because we used flume::bounded, we can non-blockingly check for messages:
        let output_msg = ssh_io.out_rx.recv().unwrap_or_else(|_| "No data".into());
        println!("Got from target: {:?}", std::str::from_utf8(&output_msg));

        // 6. Disconnect
        proxy_conn.disconnect().await?;
        // assert!(!proxy_conn.is_connected(), "Should be disconnected after calling disconnect().");

        Ok(())
    }
}
