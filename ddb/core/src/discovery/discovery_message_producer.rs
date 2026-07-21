use async_trait::async_trait;
use flume::Sender;
use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;

use crate::dbg_ctrl::TransportSpec;

pub type UserDataMap = Option<HashMap<String, String>>;

pub struct ServiceInfo {
    pub ip: Ipv4Addr,
    pub tag: String,
    pub pid: u64,
    pub hash: String,
    pub alias: String,
    pub transport: TransportSpec,
    pub user_data: UserDataMap,
}

impl ServiceInfo {
    pub fn new(
        ip: Ipv4Addr,
        tag: String,
        pid: u64,
        hash: String,
        alias: String,
        transport: TransportSpec,
        user_data: UserDataMap,
    ) -> Self {
        ServiceInfo {
            ip,
            tag,
            pid,
            hash,
            alias,
            transport,
            user_data,
        }
    }

    /// Extracts the Caladan runtime address advertised in discovery user data.
    pub fn caladan_ip(&self) -> Option<u32> {
        self.user_data.as_ref().and_then(|data| {
            data.get("caladan_ip")
                .and_then(|ip_str| ip_str.parse::<u32>().ok())
        })
    }
}
impl fmt::Display for ServiceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ServiceInfo {{ ip: {}, tag: {}, pid: {}, hash: {}, alias: {}, user_data: {:?} }}",
            self.ip, self.tag, self.pid, self.hash, self.alias, self.user_data
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
            .field("user_data", &self.user_data)
            // Note: transport is omitted as it might not implement Debug
            .finish()
    }
}

#[async_trait]
pub trait DiscoveryMessageProducer: Send + Sync {
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::Ipv4Addr};

    use super::*;
    use crate::{connection::ssh_client::SSHCred, dbg_ctrl::TransportSpec};

    fn sample_service_info() -> ServiceInfo {
        let ip = Ipv4Addr::new(127, 0, 0, 1);
        let ssh_cred = SSHCred::new(&ip.to_string(), 22, "root", None);
        ServiceInfo::new(
            ip,
            "127.0.0.1:-42".to_string(),
            42,
            "hash-a".to_string(),
            "api".to_string(),
            TransportSpec::DirectSsh(ssh_cred),
            Some(HashMap::from([("caladan_ip".to_string(), "7".to_string())])),
        )
    }

    #[test]
    fn caladan_ip_is_extracted_only_from_valid_user_data() {
        let mut info = sample_service_info();
        assert_eq!(info.caladan_ip(), Some(7));

        info.user_data = Some(HashMap::from([(
            "caladan_ip".to_string(),
            "not-a-number".to_string(),
        )]));
        assert_eq!(info.caladan_ip(), None);

        info.user_data = None;
        assert_eq!(info.caladan_ip(), None);
    }

    #[test]
    fn service_info_display_includes_public_fields() {
        let info = sample_service_info();
        let rendered = format!("{info}");

        assert!(rendered.contains("127.0.0.1"));
        assert!(rendered.contains("127.0.0.1:-42"));
        assert!(rendered.contains("pid: 42"));
        assert!(rendered.contains("hash-a"));
        assert!(rendered.contains("api"));
        assert!(rendered.contains("caladan_ip"));
    }
}
