use anyhow::{anyhow, Result};
use flume::Receiver;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::common::config::{Config as DDBConfig, StaticSessionConfig};
use crate::connection::ssh_client_channel::connect_jump_host;
use crate::discovery::{
    k8s_producer::K8sProducer, mqtt_producer::MqttProducer, runtime::DiscoveryRuntime,
    DiscoveryMessageProducer, ServiceInfo,
};
use crate::plugin::{FrameworkPlugin, ServiceDiscoveryMode};
use crate::session::factory::{DiscoveredTransportPolicy, SessionFactory};
use crate::session::supervisor::SessionSupervisor;
use crate::shutdown::ShutdownCtrl;

const SERVICE_DISCOVERY_QUEUE_CAPACITY: usize = 256;

/// Orchestrates session admission from discovery and static configuration.
pub struct DbgManager {
    supervisor: Arc<SessionSupervisor>,
    factory: SessionFactory,

    discovery: Mutex<Option<DiscoveryRuntime>>,
    admission_task: Mutex<Option<JoinHandle<()>>>,

    config: Arc<DDBConfig>,
    plugin: Arc<dyn FrameworkPlugin>,
    shutdown: Arc<ShutdownCtrl>,

    static_session_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl DbgManager {
    /// Builds the configured discovery pipeline and the transport policy its
    /// discovered sessions will use.
    async fn init_discovery(&self) -> Result<Option<DiscoveredTransportPolicy>> {
        let config = self.config.as_ref();
        let (producer_tx, producer_rx) =
            flume::bounded::<ServiceInfo>(SERVICE_DISCOVERY_QUEUE_CAPACITY);

        match self.plugin.service_discovery_mode(config) {
            ServiceDiscoveryMode::MessageBroker => {
                let mut producer = MqttProducer::from_config(
                    Arc::clone(&self.config),
                    Arc::clone(&self.shutdown),
                )?;
                let start_result = producer.start_producing(producer_tx.clone()).await;
                self.install_discovery(Box::new(producer), producer_rx)
                    .await;
                start_result
                    .map_err(|error| error.context("failed to start MQTT discovery producer"))?;
                info!("MQTT discovery producer started successfully");
                Ok(Some(DiscoveredTransportPolicy::DirectSsh {
                    port: config.ssh.port,
                    user: config.ssh.user.clone(),
                }))
            }
            ServiceDiscoveryMode::Kubernetes => {
                let service_weaver = config
                    .service_discovery
                    .as_ref()
                    .and_then(|discovery| discovery.service_weaver_conf.as_ref())
                    .ok_or_else(|| {
                        anyhow!("Kubernetes discovery requires Service Weaver configuration")
                    })?;
                let tunnel = connect_jump_host(
                    &service_weaver.jump_client_host,
                    service_weaver.jump_client_port,
                    &service_weaver.jump_client_user,
                    &service_weaver.jump_client_password,
                )
                .await?;

                let mut producer =
                    K8sProducer::new(config.clone(), service_weaver.service_name.clone());
                let start_result = producer.start_producing(producer_tx.clone()).await;
                self.install_discovery(Box::new(producer), producer_rx)
                    .await;
                start_result.map_err(|error| {
                    error.context("failed to start Kubernetes discovery producer")
                })?;
                Ok(Some(DiscoveredTransportPolicy::ProxySsh {
                    tunnel,
                    port: 22,
                    user: config.ssh.user.clone(),
                    password: Some(service_weaver.pod_ssh_password.clone()),
                }))
            }
            ServiceDiscoveryMode::None => Ok(None),
        }
    }

    async fn install_discovery(
        &self,
        producer: Box<dyn DiscoveryMessageProducer>,
        services: Receiver<ServiceInfo>,
    ) {
        self.discovery
            .lock()
            .await
            .replace(DiscoveryRuntime::new(producer, services));
    }

    /// Admits every discovered service, resolving its transport through the
    /// policy chosen at discovery start.
    fn spawn_admission_loop(
        &self,
        services: Receiver<ServiceInfo>,
        policy: DiscoveredTransportPolicy,
    ) -> JoinHandle<()> {
        let factory = self.factory.clone();
        let supervisor = Arc::downgrade(&self.supervisor);
        tokio::spawn(async move {
            while let Ok(info) = services.recv_async().await {
                let Some(supervisor) = supervisor.upgrade() else {
                    break;
                };
                debug!(?info, "received discovered service");
                let (transport, proxy_tunnel) = policy.resolve(info.ip);
                let process = match factory.create_discovered(info, transport, proxy_tunnel) {
                    Ok(process) => process,
                    Err(error) => {
                        error!(?error, "failed to construct discovered session");
                        continue;
                    }
                };
                if let Err(error) = supervisor.admit(process).await {
                    error!(?error, "failed to admit discovered session");
                }
            }
        })
    }

    async fn start_static_session(&self, session: StaticSessionConfig) -> Result<()> {
        if session.start_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(session.start_delay_ms)).await;
        }

        let tag = session.tag.clone();
        let process = self.factory.create_static(session)?;
        if let Err(error) = self.supervisor.admit(process).await {
            error!(tag, ?error, "failed to admit static session");
        }
        Ok(())
    }

    async fn init_static_sessions(self: &Arc<Self>) -> Result<()> {
        let mut delayed_handles = self.static_session_handles.lock().await;
        delayed_handles.clear();

        for session in self.config.static_sessions.clone() {
            if session.start_delay_ms == 0 {
                self.start_static_session(session).await?;
            } else {
                let manager = Arc::downgrade(self);
                delayed_handles.push(tokio::spawn(async move {
                    let Some(manager) = manager.upgrade() else {
                        return;
                    };
                    if let Err(error) = manager.start_static_session(session).await {
                        error!("Failed to start delayed static session: {:?}", error);
                    }
                }));
            }
        }
        Ok(())
    }
}

impl DbgManager {
    pub(crate) fn new(
        config: Arc<DDBConfig>,
        plugin: Arc<dyn FrameworkPlugin>,
        supervisor: Arc<SessionSupervisor>,
        factory: SessionFactory,
        shutdown: Arc<ShutdownCtrl>,
    ) -> Arc<Self> {
        Arc::new(DbgManager {
            supervisor,
            factory,
            discovery: Mutex::new(None),
            admission_task: Mutex::new(None),
            config,
            plugin,
            shutdown,
            static_session_handles: Mutex::new(Vec::new()),
        })
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.supervisor.start().await?;
        if self.config.service_discovery.is_some() {
            info!("[Service Discovery]: ENABLED. INIT service discovery...");
            if let Some(policy) = self.init_discovery().await? {
                let services = self
                    .discovery
                    .lock()
                    .await
                    .as_ref()
                    .map(|discovery| discovery.services())
                    .ok_or_else(|| anyhow!("discovery runtime was not installed"))?;
                self.admission_task
                    .lock()
                    .await
                    .replace(self.spawn_admission_loop(services, policy));
                debug!("DbgManager is now listening for discovered services.");
            }
        } else {
            info!("[Service Discovery]: DISABLED. SKIP service discovery initialization.");
        }
        if !self.config.static_sessions.is_empty() {
            info!(
                "[Static Sessions]: STARTING {} configured session(s).",
                self.config.static_sessions.len()
            );
            self.init_static_sessions().await?;
        }
        Ok(())
    }

    pub async fn cleanup(&self) {
        {
            let mut handles = self.static_session_handles.lock().await;
            for handle in handles.drain(..) {
                handle.abort();
            }
        }

        if let Some(discovery) = &mut *self.discovery.lock().await {
            debug!("Shutting down service discovery...");
            if let Err(error) = discovery.shutdown().await {
                error!(?error, "service discovery shutdown failed");
            }
        }
        if let Some(task) = self.admission_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }

        self.supervisor.shutdown().await;
        debug!("[DbgManager]: Cleanup complete.");
    }
}
