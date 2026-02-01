use anyhow::{Context, Result};
use rumqttc::{Client, MqttOptions, QoS};
use std::time::Duration;
use tokio::{
    sync::watch,
    time::{self, Instant},
};
use tracing::{debug, error};

use crate::{common::Config, shutdown::get_shutdown_ctrl};

pub struct AsyncDiscoverClient {
    client: rumqttc::AsyncClient,
    el: rumqttc::EventLoop,
}

impl AsyncDiscoverClient {
    pub fn new(client_id: &str, host: &str, port: u16) -> Self {
        use crate::common::{sd_defaults, utils};

        let mut mqttoptions = MqttOptions::new(client_id, host, port);
        mqttoptions.set_transport(utils::mqtt::str_to_transport(
            sd_defaults::BROKER_MSG_TRANSPORT,
        ));
        mqttoptions.set_keep_alive(Duration::from_secs(5));

        let (client, el) = rumqttc::AsyncClient::new(mqttoptions, 100);
        AsyncDiscoverClient { client, el }
    }

    pub async fn check_broker_online(&mut self) -> Result<()> {
        let start_time = Instant::now();
        let user_defined_timeout = Config::global()
            .service_discovery
            .as_ref()
            .and_then(|sd_cfg| sd_cfg.broker.max_timeout_secs);
        let timeout = match user_defined_timeout {
            Some(secs) => Duration::from_secs(secs),
            None => Duration::from_secs(30),
        };

        loop {
            // Try connecting and poll for events
            match time::timeout(Duration::from_secs(1), self.el.poll()).await {
                Ok(Ok(_)) => {
                    return Ok(());
                }
                _ => {
                    debug!("Broker is offline, retrying...");
                }
            }

            if get_shutdown_ctrl().should_shutdown() {
                return Err(anyhow::anyhow!("Expect shutdown."))
                    .context("Aborting broker connection attempts due to shutdown signal.");
            }

            if start_time.elapsed() >= timeout {
                let (addr, port) = self.el.mqtt_options.broker_address();
                return Err(anyhow::anyhow!("Exceeded retry timeout, broker is offline")).context(
                    format!(
                        "Failed to connect to service discovery broker at {}:{} after {} secs.",
                        addr,
                        port,
                        timeout.as_secs()
                    ),
                );
            }

            // Wait before retrying
            time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn subscribe(&mut self, topic: &str, qos: QoS) -> Result<()> {
        self.client.subscribe(topic, qos).await?;
        Ok(())
    }

    #[allow(unused)]
    pub async fn publish(&self, topic: &str, qos: QoS, retain: bool, payload: &str) -> Result<()> {
        self.client.publish(topic, qos, retain, payload).await?;
        Ok(())
    }

    #[inline]
    pub async fn handle(
        &mut self,
        sender: flume::Sender<rumqttc::Event>,
        mut sig_stop: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut failure_count: u32 = 0;
        loop {
            tokio::select! {
                event = self.el.poll() => {
                    match event {
                        Ok(event) => {
                            sender.send_async(event).await?;
                        },
                        Err(e) => {
                            failure_count += 1;
                            error!("Error to poll from broker: {:?}", e);
                            if failure_count >= 5 {
                                return Err(anyhow::anyhow!("Exceeded maximum poll failures."));
                            }
                        }
                    }
                },
                _ = sig_stop.changed() => {
                    if *sig_stop.borrow() {
                        debug!("Stopping AsyncDiscoverClient polling...");
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub struct DiscoverClient {
    client: Client,
    connection: rumqttc::Connection,
}

#[allow(dead_code)]
impl DiscoverClient {
    pub fn new(client_id: &str, host: &str, port: u16) -> Self {
        let mut mqttoptions = MqttOptions::new(client_id, host, port);
        mqttoptions.set_keep_alive(Duration::from_secs(5));

        let (client, connection) = Client::new(mqttoptions, 10);

        DiscoverClient { client, connection }
    }

    pub fn subscribe(&mut self, topic: &str, qos: QoS) {
        self.client.subscribe(topic, qos).unwrap();
    }

    pub fn publish(&mut self, topic: &str, qos: QoS, retain: bool, payload: &str) {
        self.client.publish(topic, qos, retain, payload).unwrap();
    }

    #[inline]
    pub fn handle<F>(&mut self) -> Result<rumqttc::Event> {
        match self.connection.recv() {
            Ok(notification) => Ok(notification?),
            Err(e) => Err(anyhow::anyhow!("Error: {:?}", e)),
        }
    }
}
