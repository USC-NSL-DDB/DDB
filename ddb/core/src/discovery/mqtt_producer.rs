use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    net::Ipv4Addr,
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result};
use flume::Sender;
use rumqttc::{Event, Packet};
use tokio::task::JoinHandle;
use tracing::{debug, info};

use super::{
    broker::{BrokerInfo, MessageBroker},
    discovery_message_producer::{DiscoveryMessageProducer, ServiceInfo},
};
use crate::{
    common::sd_defaults, connection::ssh_client::SSHCred, dbg_ctrl::TransportSpec,
    discovery::subscriber::AsyncDiscoverClient,
};

fn write_config(broker: &BrokerInfo, config_path: &str) -> Result<()> {
    let path = Path::new(config_path);
    debug!("Writing broker config to {:?}", path);

    // Create parent directory if it doesn't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = File::create(path)?;

    writeln!(
        file,
        "{}://{}:{}\n{}\n",
        sd_defaults::BROKER_MSG_TRANSPORT,
        broker.hostname,
        broker.port,
        sd_defaults::T_SERVICE_DISCOVERY
    )?;

    Ok(())
}

/// A Producer that uses MQTT (via `AsyncDiscoverClient`) to receive
/// `ServiceInfo` events and send them through a channel.
pub struct MqttProducer<'a> {
    /// If you want this producer to also own and manage the broker lifecycle,
    /// store it here. If `None`, we assume the broker is managed externally.
    managed_broker: Option<Box<dyn MessageBroker>>,

    /// Keep track of spawned tasks for `start_producing`. We’ll abort them in `stop_producing`.
    handles: Vec<JoinHandle<()>>,

    config: &'a crate::common::config::Config,
}

impl<'a> MqttProducer<'a> {
    /// Create a new MqttProducer, optionally with an owned broker.
    pub fn new(
        managed_broker: Option<Box<dyn MessageBroker>>,
        config: &'a crate::common::config::Config,
    ) -> Self {
        Self {
            managed_broker,
            handles: Vec::new(),
            config,
        }
    }
    fn monitor(
        &self,
        mut client: AsyncDiscoverClient,
        sender: Sender<rumqttc::Event>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(error) = client.handle(sender).await {
                debug!(?error, "MQTT discovery monitor stopped");
            }
        })
    }
}

pub struct MqttPayload {
    pub ip: Ipv4Addr,
    pub tag: String,
    pub pid: u64,
    pub hash: String,
    pub alias: String,
    pub user_data: Option<HashMap<String, String>>,
}

impl TryFrom<&str> for MqttPayload {
    type Error = anyhow::Error;

    fn try_from(payload: &str) -> Result<Self> {
        let mut parts = payload.split(':');
        let ip_raw = parts
            .next()
            .context("MQTT discovery payload is missing its IP address")?;
        parts
            .next()
            .context("MQTT discovery payload is missing its endpoint field")?;
        let pid_raw = parts
            .next()
            .context("MQTT discovery payload is missing its process ID")?;

        let ip = Ipv4Addr::from(
            ip_raw
                .parse::<u32>()
                .context("MQTT discovery payload has an invalid IP address")?,
        );
        let pid = pid_raw
            .parse::<u64>()
            .context("MQTT discovery payload has an invalid process ID")?;
        let tag = format!("{}:-{}", ip, pid);

        let remaining = parts.collect::<Vec<_>>();
        let identifier = remaining
            .first()
            .copied()
            .filter(|value| !value.starts_with('{'));
        let (hash, alias) = identifier
            .map(|identifier| {
                let (hash, alias) = identifier.split_once('=').unwrap_or((identifier, "app"));
                (hash.to_string(), alias.to_string())
            })
            .unwrap_or_default();

        let user_data = remaining
            .last()
            .copied()
            .filter(|value| value.starts_with('{'))
            .map(|value| {
                value
                    .trim_start_matches('{')
                    .trim_end_matches('}')
                    .split(',')
                    .filter_map(|pair| {
                        let (key, value) = pair.trim().split_once('=').unwrap_or((pair, ""));
                        let key = key.trim();
                        (!key.is_empty()).then(|| (key.to_string(), value.trim().to_string()))
                    })
                    .collect::<HashMap<String, String>>()
            });

        Ok(Self {
            ip,
            tag,
            pid,
            hash,
            alias,
            user_data,
        })
    }
}

#[axum::async_trait]
impl<'a> DiscoveryMessageProducer for MqttProducer<'a> {
    /// Start “producing” by:
    /// 1. Optionally starting our broker,
    /// 2. Creating an AsyncDiscoverClient,
    /// 3. Subscribing to the desired topic,
    /// 4. Spawning a monitor task that feeds an internal channel with MQTT events,
    /// 5. Spawning consumer tasks that parse events and send `ServiceInfo` into `tx`.
    async fn start_producing(&mut self, tx: Sender<ServiceInfo>) -> Result<()> {
        // 1. Resolve the configured endpoint and start the broker if we manage it.
        let sd_config = self
            .config
            .service_discovery
            .as_ref()
            .context("MQTT discovery requires broker configuration")?;
        let broker_config = &sd_config.broker;
        if let Some(broker) = &mut self.managed_broker {
            info!("Starting managed broker...");
            let broker_info = BrokerInfo {
                hostname: broker_config.hostname.clone(),
                port: broker_config.port,
                broker_config: broker_config.managed.clone(),
            };

            // write broker config file, so that client (debuggee DDB connector)
            // can read it and figure out how to connect to the broker.
            write_config(&broker_info, &sd_config.config_path)?;

            broker
                .start(&broker_info)
                .context("Failed to start managed broker")?;
        }

        // 2. Create an AsyncDiscoverClient and subscribe
        let mut client = AsyncDiscoverClient::new(
            sd_defaults::CLIENT_ID,
            &broker_config.hostname,
            broker_config.port,
        );
        let connect_timeout = Duration::from_secs(broker_config.max_timeout_secs.unwrap_or(30));
        client.check_broker_online(connect_timeout).await?;
        client
            .subscribe(sd_defaults::T_SERVICE_DISCOVERY, rumqttc::QoS::ExactlyOnce)
            .await
            .context("failed to subscribe to the service-discovery topic")?;
        info!("Successfully connected and subscribed to broker");
        let (event_sender, event_receiver) = flume::bounded(1024);
        let monitor_handle = self.monitor(client, event_sender.clone());
        self.handles.push(monitor_handle);

        // 3. Spawn consumer tasks that read from event_receiver and forward to `tx`.
        let concurrency = 3;
        for _ in 0..concurrency {
            let event_rx = event_receiver.clone();
            let tx_clone = tx.clone();

            let ssh_port = self.config.ssh.port;
            let ssh_user = self.config.ssh.user.clone();

            let handle = tokio::spawn(async move {
                while let Ok(event) = event_rx.recv_async().await {
                    if let Event::Incoming(Packet::Publish(publish)) = event {
                        if let Ok(payload_str) = std::str::from_utf8(&publish.payload) {
                            let mqtt_payload = match MqttPayload::try_from(payload_str) {
                                Ok(payload) => payload,
                                Err(error) => {
                                    debug!(
                                        ?error,
                                        payload = %payload_str,
                                        "ignoring malformed MQTT discovery payload"
                                    );
                                    continue;
                                }
                            };
                            let ssh_cred = SSHCred::new(
                                mqtt_payload.ip.to_string().as_str(),
                                ssh_port,
                                ssh_user.as_str(),
                                None,
                            );
                            let info = ServiceInfo::new(
                                mqtt_payload.ip,
                                mqtt_payload.tag,
                                mqtt_payload.pid,
                                mqtt_payload.hash,
                                mqtt_payload.alias,
                                TransportSpec::DirectSsh(ssh_cred),
                                mqtt_payload.user_data,
                            );

                            if let Err(error) = tx_clone.send_async(info).await {
                                debug!(?error, "discovery receiver closed");
                                break;
                            }
                        } else {
                            debug!("Ignoring invalid UTF-8 payload.");
                        }
                    }
                }
            });
            self.handles.push(handle);
        }

        Ok(())
    }

    /// Stop all owned tasks, then stop the broker if we started it.
    async fn stop_producing(&mut self) -> Result<()> {
        debug!("Stopping MqttProducer…");
        // Abort first so no task keeps running while another join is awaited.
        for handle in &self.handles {
            handle.abort();
        }
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }

        // Stop the broker if we own it
        if let Some(broker) = &mut self.managed_broker {
            debug!("Stopping managed broker…");
            broker.stop().context("Failed to stop managed broker")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::Ipv4Addr};

    use super::*;

    #[test]
    fn mqtt_payload_parses_identifier_and_user_data() {
        let ip = Ipv4Addr::new(10, 0, 0, 5);
        let payload = format!(
            "{}:ignored:42:hash-a=api:{{caladan_ip=7,role=leader}}",
            u32::from(ip)
        );

        let parsed = MqttPayload::try_from(payload.as_str()).expect("payload should be valid");

        assert_eq!(parsed.ip, ip);
        assert_eq!(parsed.tag, "10.0.0.5:-42");
        assert_eq!(parsed.pid, 42);
        assert_eq!(parsed.hash, "hash-a");
        assert_eq!(parsed.alias, "api");
        assert_eq!(
            parsed
                .user_data
                .as_ref()
                .and_then(|data| data.get("caladan_ip"))
                .map(String::as_str),
            Some("7")
        );
        assert_eq!(
            parsed
                .user_data
                .as_ref()
                .and_then(|data| data.get("role"))
                .map(String::as_str),
            Some("leader")
        );
    }

    #[test]
    fn mqtt_payload_defaults_hash_alias_and_user_data_when_absent() {
        let ip = Ipv4Addr::new(127, 0, 0, 1);
        let payload = format!("{}:ignored:9", u32::from(ip));

        let parsed = MqttPayload::try_from(payload.as_str()).expect("payload should be valid");

        assert_eq!(parsed.ip, ip);
        assert_eq!(parsed.tag, "127.0.0.1:-9");
        assert_eq!(parsed.pid, 9);
        assert!(parsed.hash.is_empty());
        assert!(parsed.alias.is_empty());
        assert!(parsed.user_data.is_none());
    }

    #[test]
    fn mqtt_payload_rejects_missing_and_invalid_required_fields() {
        assert!(MqttPayload::try_from("1:missing-pid").is_err());
        assert!(MqttPayload::try_from("not-an-ip:ignored:42").is_err());
        assert!(MqttPayload::try_from("1:ignored:not-a-pid").is_err());
    }

    #[test]
    fn mqtt_payload_accepts_user_data_without_an_identifier() {
        let parsed = MqttPayload::try_from("2130706433:ignored:9:{role=leader}")
            .expect("payload should be valid");

        assert!(parsed.hash.is_empty());
        assert!(parsed.alias.is_empty());
        assert_eq!(
            parsed
                .user_data
                .as_ref()
                .and_then(|data| data.get("role"))
                .map(String::as_str),
            Some("leader")
        );
    }

    #[test]
    fn write_config_persists_broker_endpoint_and_topic() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let config_path = dir.path().join("service_discovery").join("broker.conf");
        let broker = BrokerInfo {
            hostname: "broker.local".to_string(),
            port: 1883,
            broker_config: None,
        };

        write_config(&broker, config_path.to_str().unwrap()).expect("config should be written");

        let contents = fs::read_to_string(config_path).expect("config should exist");
        assert!(contents.contains(&format!(
            "{}://broker.local:1883",
            sd_defaults::BROKER_MSG_TRANSPORT
        )));
        assert!(contents.contains(sd_defaults::T_SERVICE_DISCOVERY));
    }
}
