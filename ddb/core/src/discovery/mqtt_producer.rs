use std::{collections::HashMap, net::Ipv4Addr};

use anyhow::{Context, Result};
use flume::Sender;
use rumqttc::{Event, Packet};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use super::{
    broker::{BrokerInfo, MessageBroker},
    discovery_message_producer::{DiscoveryMessageProducer, ServiceInfo},
};
use crate::{
    common::sd_defaults, connection::ssh_client::SSHCred, dbg_ctrl::SSHAttachController,
    discovery::subscriber::AsyncDiscoverClient,
};

/// A Producer that uses MQTT (via `AsyncDiscoverClient`) to receive
/// `ServiceInfo` events and send them through a channel.
pub struct MqttProducer {
    /// If you want this producer to also own and manage the broker lifecycle,
    /// store it here. If `None`, we assume the broker is managed externally.
    managed_broker: Option<Box<dyn MessageBroker>>,

    /// We’ll use this watch channel to gracefully tell our tasks to stop.
    sig_stop: watch::Sender<bool>,

    /// Keep track of spawned tasks for `start_producing`. We’ll abort them in `stop_producing`.
    handles: Vec<JoinHandle<()>>,

    config: crate::common::config::Config,
}

impl MqttProducer {
    /// Create a new MqttProducer, optionally with an owned broker.
    pub fn new(
        managed_broker: Option<Box<dyn MessageBroker>>,
        config: crate::common::config::Config,
    ) -> Self {
        let (sig_stop, _) = watch::channel(false);
        Self {
            managed_broker,
            sig_stop,
            handles: Vec::new(),
            config,
        }
    }
    fn monitor(
        &self,
        mut client: AsyncDiscoverClient,
        sender: Sender<rumqttc::Event>,
    ) -> tokio::task::JoinHandle<()> {
        let stop_rx = self.sig_stop.subscribe();
        let sender = sender.clone();

        tokio::spawn(async move {
            // We should respect ExactlyOnce semantics.
            if let Ok(_) = client
                .subscribe(sd_defaults::T_SERVICE_DISCOVERY, rumqttc::QoS::ExactlyOnce)
                .await
            {
                if let Err(e) = client.handle(sender, stop_rx).await {
                    error!("Client handler error: {}", e);
                }
            } else {
                debug!(
                    "Failed to subscribe to topic: {}",
                    sd_defaults::T_SERVICE_DISCOVERY
                );
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
impl From<&str> for MqttPayload {
    fn from(s: &str) -> Self {
        let parts: Vec<&str> = s.split(':').collect();

        let ip_int: u32 = parts[0].parse().unwrap();
        let ip = Ipv4Addr::from(ip_int);
        let pid = parts[2].parse().unwrap();

        let tag = format!("{}:-{}", ip, pid);

        let (hash, alias) = parts
            .get(3)
            .map(|identifier| {
                let identifier = *identifier;
                let identifier_parts: Vec<_> = identifier.split('=').collect();
                let hash = identifier_parts[0];
                let alias = identifier_parts.get(1).unwrap_or(&"app").to_string();
                (hash.to_string(), alias)
            })
            .unwrap_or((String::new(), String::new()));
        
        let user_data = parts.last().map(|&user_data| {
            // if doesn't start with "{", meaning it is not a user_data field, then ignore it.
            // we assume the user_data field is the last one in the payload if it exists.
            user_data.starts_with("{").then(|| {
                let user_data = user_data.trim_start_matches('{').trim_end_matches('}');
                // example payload:
                // {key1=value1,key2=value2}
                user_data.split(",").map(|kv| {
                    let kv_parts: Vec<_> = kv.trim().split('=').collect();
                    let key = kv_parts[0].trim().to_string();
                    let value = kv_parts.get(1).unwrap_or(&"").trim().to_string();
                    (key, value)
                }).collect::<HashMap<String, String>>()                
            })
        }).flatten();

        MqttPayload {
            ip,
            tag,
            pid,
            hash,
            alias,
            user_data
        }
    }
}

#[axum::async_trait]
impl DiscoveryMessageProducer for MqttProducer {
    /// Start “producing” by:
    /// 1. Optionally starting our broker,
    /// 2. Creating an AsyncDiscoverClient,
    /// 3. Subscribing to the desired topic,
    /// 4. Spawning a monitor task that feeds an internal channel with MQTT events,
    /// 5. Spawning consumer tasks that parse events and send `ServiceInfo` into `tx`.
    async fn start_producing(
        &mut self,
        tx: Sender<ServiceInfo>,
    ) -> Result<()> {
        // 1. Start the broker if we manage it
        if let Some(broker) = &self.managed_broker {
            info!("Starting managed broker...");
            let broker_info = BrokerInfo {
                hostname: sd_defaults::DEFAULT_BROKER_HOSTNAME.to_string(),
                port: sd_defaults::BROKER_PORT,
            };

            // Get the config path from the configuration
            let config_path = self
                .config
                .service_discovery
                .as_ref()
                .map(|sd| sd.config_path.as_str())
                .unwrap_or(sd_defaults::SERVICE_DISCOVERY_INI_FILEPATH);

            broker
                .start(&broker_info, config_path)
                .context("Failed to start managed broker")?;
        }

        // 2. Create an AsyncDiscoverClient and subscribe
        let mut client = AsyncDiscoverClient::new(
            sd_defaults::CLIENT_ID,
            sd_defaults::DEFAULT_BROKER_HOSTNAME,
            sd_defaults::BROKER_PORT,
        );
        if let Err(e) = client.check_broker_online().await {
            return Err(anyhow::anyhow!("Failed to connect to broker: {}", e));
        }
        info!("Successfully connected to broker");
        let (event_sender, event_receiver) = flume::bounded(1024);
        self.monitor(client, event_sender.clone());

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
                            let mqtt_payload = MqttPayload::from(payload_str);
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
                                Box::new(SSHAttachController::new(ssh_cred)),
                                mqtt_payload.user_data,
                            );

                            if let Err(e) = tx_clone.send_async(info).await {
                                error!("Failed to send ServiceInfo: {}", e);
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

    /// Stop producing:
    /// 1. Abort all spawned tasks,
    /// 2. Signal “stop” to the monitor task,
    /// 3. Stop the broker if we started it.
    async fn stop_producing(&mut self) -> Result<()> {
        //
        // 1. Abort tasks
        //
        for handle in &self.handles {
            handle.abort();
        }
        self.handles.clear();

        //
        // 2. Send stop signal to the monitor task
        //
        let _ = self.sig_stop.send(true);

        //
        // 3. Stop broker if we own it
        //
        if let Some(broker) = &self.managed_broker {
            broker.stop().context("Failed to stop managed broker")?;
        }

        Ok(())
    }
}
