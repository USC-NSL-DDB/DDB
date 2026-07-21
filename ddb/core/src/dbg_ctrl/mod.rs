mod dbg_ctrl;
mod local_ctrl;
mod mock_ctrl;

pub use dbg_ctrl::{DebuggerTransport, DebuggerTransportHandle};

use dbg_ctrl::RemoteTransport;
use local_ctrl::LocalProcessController;
use mock_ctrl::MockAttachController;

use std::sync::Arc;

use anyhow::{anyhow, Result};
use russh::client::Handle;

use crate::{
    common::mock_fixture::MockSessionConfig,
    connection::{
        ssh_client::{SSHConnection, SSHCred},
        ssh_client_channel::{SSHProxyClientHandler, SSHProxyConnection, SSHProxyCred},
    },
};

/// Shared authenticated tunnel used by proxy SSH transports.
pub type ProxyTunnel = Arc<Handle<SSHProxyClientHandler>>;

/// Pure description of how a debugger session should be transported.
#[derive(Debug, Clone)]
pub enum TransportSpec {
    DirectSsh(SSHCred),
    ProxySsh(SSHProxyCred),
    Local,
    Mock { config: MockSessionConfig, pid: u64 },
}

pub fn build_transport(
    spec: &TransportSpec,
    proxy_tunnel: Option<ProxyTunnel>,
) -> Result<DebuggerTransportHandle> {
    match spec {
        TransportSpec::DirectSsh(credentials) => Ok(Box::new(RemoteTransport::new(
            SSHConnection::new(credentials.clone(), None),
        ))),
        TransportSpec::ProxySsh(credentials) => {
            let tunnel = proxy_tunnel
                .ok_or_else(|| anyhow!("proxy SSH transport requires a bastion session"))?;
            Ok(Box::new(RemoteTransport::new(SSHProxyConnection::new(
                tunnel,
                credentials.clone(),
                None,
            ))))
        }
        TransportSpec::Local => Ok(Box::new(LocalProcessController::new())),
        TransportSpec::Mock { config, pid } => {
            Ok(Box::new(MockAttachController::new(config.clone(), *pid)))
        }
    }
}
