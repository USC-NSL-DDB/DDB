use async_trait::async_trait;
use flume::Sender;
use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;

use crate::dbg_ctrl::DbgController;

pub type UserDataMap = Option<HashMap<String, String>>;

pub struct ServiceInfo {
    pub ip: Ipv4Addr,
    pub tag: String,
    pub pid: u64,
    pub hash: String,
    pub alias: String,
    pub ssh_controller: DbgController,
    pub user_data: UserDataMap,
}

impl ServiceInfo {
    pub fn new(
        ip: Ipv4Addr,
        tag: String,
        pid: u64,
        hash: String,
        alias: String,
        ssh_controller: DbgController,
        user_data: UserDataMap,
    ) -> Self {
        ServiceInfo {
            ip,
            tag,
            pid,
            hash,
            alias,
            ssh_controller,
            user_data,
        }
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
            // Note: ssh_controller is omitted as it might not implement Debug
            .finish()
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ServiceMeta {
    pub ip: Ipv4Addr,
    pub tag: String,
    pub pid: u64,
    pub hash: String,
    pub alias: String,
    pub user_data: UserDataMap,
}

impl ServiceMeta {
    pub fn new(
        ip: Ipv4Addr,
        tag: String,
        pid: u64,
        hash: String,
        alias: String,
        user_data: UserDataMap,
    ) -> Self {
        ServiceMeta {
            ip,
            tag,
            pid,
            hash,
            alias,
            user_data,
        }
    }

    pub fn from_service_info(info: &ServiceInfo) -> Self {
        ServiceMeta {
            ip: info.ip,
            tag: info.tag.clone(),
            pid: info.pid,
            hash: info.hash.clone(),
            alias: info.alias.clone(),
            user_data: info.user_data.clone(),
        }
    }

    pub fn from_service_info_owned(info: ServiceInfo) -> Self {
        ServiceMeta {
            ip: info.ip,
            tag: info.tag,
            pid: info.pid,
            hash: info.hash,
            alias: info.alias,
            user_data: info.user_data,
        }
    }
}

impl From<&ServiceInfo> for ServiceMeta {
    fn from(info: &ServiceInfo) -> Self {
        ServiceMeta::from_service_info(info)
    }
}

impl From<ServiceInfo> for ServiceMeta {
    fn from(info: ServiceInfo) -> Self {
        ServiceMeta::from_service_info_owned(info)
    }
}

impl fmt::Display for ServiceMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ServiceMeta {{ ip: {}, tag: {}, pid: {}, hash: {}, alias: {} }}",
            self.ip, self.tag, self.pid, self.hash, self.alias
        )
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
    use crate::{connection::ssh_client::SSHCred, dbg_ctrl::SSHAttachController};

    fn sample_service_info() -> ServiceInfo {
        let ip = Ipv4Addr::new(127, 0, 0, 1);
        let ssh_cred = SSHCred::new(&ip.to_string(), 22, "root", None);
        ServiceInfo::new(
            ip,
            "127.0.0.1:-42".to_string(),
            42,
            "hash-a".to_string(),
            "api".to_string(),
            Box::new(SSHAttachController::new(ssh_cred)),
            Some(HashMap::from([("caladan_ip".to_string(), "7".to_string())])),
        )
    }

    #[test]
    fn service_meta_from_service_info_copies_metadata_fields() {
        let info = sample_service_info();

        let meta = ServiceMeta::from_service_info(&info);

        assert_eq!(meta.ip, Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(meta.tag, "127.0.0.1:-42");
        assert_eq!(meta.pid, 42);
        assert_eq!(meta.hash, "hash-a");
        assert_eq!(meta.alias, "api");
        assert_eq!(
            meta.user_data
                .as_ref()
                .and_then(|data| data.get("caladan_ip"))
                .map(String::as_str),
            Some("7")
        );
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
