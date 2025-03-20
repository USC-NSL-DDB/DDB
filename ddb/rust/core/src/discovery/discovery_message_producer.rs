use async_trait::async_trait;
use flume::Sender;
use std::net::Ipv4Addr;
use std::fmt;

use crate::dbg_ctrl::DbgController;

pub struct ServiceInfo
{
    pub ip: Ipv4Addr,
    pub tag: String,
    pub pid: u64,
    pub hash: String,
    pub alias: String,
    pub ssh_controller: DbgController,
}

impl ServiceInfo
{
    pub fn new(ip: Ipv4Addr, tag: String, pid: u64, hash: String, alias: String, ssh_controller: DbgController) -> Self {
        ServiceInfo {
            ip,
            tag,
            pid,
            hash,
            alias,
            ssh_controller,
        }
    }
}
impl fmt::Display for ServiceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ServiceInfo {{ ip: {}, tag: {}, pid: {}, hash: {}, alias: {} }}",
            self.ip, self.tag, self.pid, self.hash, self.alias
        )
    }
}
impl fmt::Debug for ServiceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceInfo")
            .field("ip", &self.ip)
            .field("tag", &self.tag)
            .field("pid", &self.pid)
            .field("hash", &self.hash)
            .field("alias", &self.alias)
            // Note: ssh_controller is omitted as it might not implement Debug
            .finish()
    }
}
#[async_trait]
pub trait DiscoveryMessageProducer: Send + Sync
{
    /// Start producing events.
    ///
    /// * `tx`: A `flume::Sender` where this producer should push events as they arrive.
    /// * The producer can spawn its own background tasks or maintain internal state.
    /// * Return an error if startup fails (e.g., can’t connect to broker).
    async fn start_producing(&mut self, tx: Sender<ServiceInfo>) -> anyhow::Result<()>;

    /// Stop producing events.
    ///
    /// * Perform a graceful shutdown of your background tasks, broker connection, etc.
    /// * After calling `stop_producing`, the producer should no longer push into `tx`.
    async fn stop_producing(&mut self) -> anyhow::Result<()>;
}
