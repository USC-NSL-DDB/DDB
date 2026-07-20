use anyhow::{Context, Result};
use rumqttc::{MqttOptions, QoS};
use std::time::Duration;
use tokio::time::{self, Instant};
use tracing::{debug, error};

use crate::shutdown::get_shutdown_ctrl;

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

    pub async fn check_broker_online(&mut self, timeout: Duration) -> Result<()> {
        let start_time = Instant::now();

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
    pub async fn handle(&mut self, sender: flume::Sender<rumqttc::Event>) -> Result<()> {
        let mut failure_count: u32 = 0;
        loop {
            match self.el.poll().await {
                Ok(event) => {
                    failure_count = 0;
                    sender.send_async(event).await?;
                }
                Err(error) => {
                    failure_count += 1;
                    error!(?error, failure_count, "failed to poll discovery broker");
                    if failure_count >= 5 {
                        return Err(anyhow::anyhow!("exceeded maximum broker poll failures"));
                    }
                }
            }
        }
    }
}
