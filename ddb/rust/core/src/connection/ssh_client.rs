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

use super::{RemoteConnectable, SSHIo};
use crate::{
    common::default_vals::DEFAULT_SSH_PRIVATE_KEY_PATH,
    dbg_ctrl::{InputReceiver, OutputSender},
};

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
    async fn try_connect(&mut self) -> Result<()> {
        // TODO: use more sophisticated authentication handling...
        let mut session = client::connect(
            self.config.clone(),
            (self.cred.hostname.clone(), self.cred.port),
            SSHClientHandler(self.exited_sender.clone()),
        )
        .await?;
        let key_pair = load_secret_key(self.cred.private_key_path.clone(), None)?;
        session
            .authenticate_publickey(
                self.cred.username.clone(),
                PrivateKeyWithHashAlg::new(
                    Arc::new(key_pair),
                    session.best_supported_rsa_hash().await.unwrap().flatten(),
                ),
            )
            .await?;
        self.session = Some(session);
        Ok(())
    }

    async fn poll(mut ssh_chan: Channel<Msg>, in_rx: InputReceiver, out_tx: OutputSender) {
        loop {
            tokio::select! {
                Ok(data) = in_rx.recv_async() => {
                    debug!("Sending data: {:?}", data);
                    match ssh_chan.data(data.as_ref()).await {
                        Ok(_) => {}
                        Err(e) => {
                            // if in_rx.is_closed() {
                            //     debug!("Input channel closed.");
                            //     break;
                            // }
                            debug!("Error sending input data: {}", e);
                        }
                    }
                }
                Some(msg) = ssh_chan.wait() => {
                    match msg {
                        ChannelMsg::Data { ref data } => {
                            // debug!("Read data: {:?}", std::str::from_utf8(data.to_vec().as_slice()));
                            if let Err(e) = out_tx.send_async(Bytes::from(data.to_vec())).await {
                                debug!("Failed to send output data: {}", e);
                                break;
                            }
                        }
                        ChannelMsg::Eof => {}
                        ChannelMsg::ExitStatus { exit_status: _ } => {
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
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
                Err(e) => {
                    debug!("Failed to connect: {}. Retrying...", e);
                }
            }
            time::sleep(Duration::from_millis(500)).await;
            counter += 1;
        }
        Ok(())
    }

    async fn start(&mut self, cmd: &str) -> Result<SSHIo> {
        if let Some(s) = &self.session {
            let chan = s.channel_open_session().await?;
            chan.exec(true, cmd).await?;

            // Create channel for sending data to SSH
            // This is a workaround for the issue that the SSH library doesn't provide a way to
            // send data to the remote program in a thread-safe concurrent manner.
            let (in_tx, in_rx) = flume::bounded::<Bytes>(1024);
            let (out_tx, out_rx) = flume::bounded::<Bytes>(1024);

            self.poll_handle = Some(tokio::spawn(Self::poll(chan, in_rx, out_tx)));

            return Ok(SSHIo { in_tx, out_rx });
        }
        Err(anyhow::anyhow!("Session is not available."))
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(s) = self.session.take() {
            s.disconnect(Disconnect::ByApplication, "Exit from DCore.", "en")
                .await?;
        }

        if let Some(h) = self.poll_handle.take() {
            h.abort();
        }
        Ok(())
    }

    #[inline]
    fn is_connected(&self) -> bool {
        // Note: this is not a perfect check, but it should be good enough for now.
        // Session keeps connected even if the remote program is closed.
        // Therefore, we need to check the exited flag.
        !self.exited.borrow().clone()
    }
}

// #[async_trait]
// impl RemoteWritable for SSHConnection {
//     #[inline]
//     async fn write(&self, data: Bytes) -> Result<()> {
//         if let Some(c) = &self.in_tx {
//             c.send_async(data).await?;
//         }
//         Ok(())
//     }
// }

// #[async_trait]
// impl RemoteReadable for SSHConnection {
//     #[inline]
//     async fn read(&mut self) -> Result<Bytes> {
//         if let Some(c) = &mut self.channel {
//             loop {
//                 let msg = c.wait().await;
//                 match msg {
//                     Some(msg) => match msg {
//                         ChannelMsg::Data { ref data } => {
//                             debug!("Data: {:?}", data);
//                             return Ok(Bytes::from(data.to_vec()));
//                         }
//                         _ => {
//                             debug!("Other: {:?}", msg);
//                             // return Ok(String::from_utf8(msg).unwrap());
//                             continue;
//                         }
//                     },
//                     None => {
//                         debug!("Error reading");
//                         anyhow::bail!("Error reading.");
//                     }
//                 }
//             }
//         }
//         anyhow::bail!("Channel is not available.")
//     }
// }

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use std::sync::Arc;
//     use tokio::runtime::Runtime;

//     use crate::common::default_vals::DEFAULT_SSH_USER;

//     #[test]
//     fn test_ssh_connection() {
//         let rt = Runtime::new().unwrap();
//         rt.block_on(async {
//             let cred = SSHCred::new("127.0.0.1", 22, &DEFAULT_SSH_USER, None);
//             let config = Arc::new(Config::default());
//             // let (tx, mut rx) = tokio::sync::mpsc::channel::<Bytes>(1024);
//             let (tx, mut rx) = flume::bounded::<Bytes>(1024);
//             let mut conn = SSHConnection::new(cred, Some(config));
//             conn.connect().await.unwrap();
//             assert!(conn.is_connected());
//             conn.start("echo $USER").await.unwrap();

//             let res = rx.recv_async().await.unwrap();
//             let res = std::str::from_utf8(res.as_ref()).unwrap();
//             assert_eq!(res.trim(), std::env::var("USER").unwrap());
//             conn.disconnect().await.unwrap();
//             assert_eq!(conn.is_connected(), false);
//         });
//     }
// }
