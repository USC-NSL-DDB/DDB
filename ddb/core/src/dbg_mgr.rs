use anyhow::{anyhow, bail, Context, Result};
use russh::client::Config as RusshClientConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::context::AppContext;
use crate::discovery::{
    broker::{EMQXBroker, MessageBroker, MosquittoBroker},
    runtime::DiscoveryRuntime,
};
use crate::feature::proclet_ctrl::{ProcletCtrlClient, QueryProcletResp};
use crate::plugin::{FrameworkPlugin, ServiceDiscoveryMode};
use crate::session::{factory::SessionFactory, supervisor::SessionSupervisor};
use crate::{
    common::{
        self,
        config::{Config as DDBConfig, StaticSessionConfig},
    },
    discovery::DiscoveryMessageProducer,
};

const SERVICE_DISCOVERY_QUEUE_CAPACITY: usize = 256;

/// The manager that can handle multiple producers, each sending discovered services.
pub struct DbgManager {
    supervisor: Arc<SessionSupervisor>,
    factory: SessionFactory,

    discovery: Mutex<Option<DiscoveryRuntime>>,

    // This should be non-null if the framework is Nu/Quicksand and migration support is enabled.
    proclet_ctrl: Option<ProcletCtrlClient>,

    config: Arc<DDBConfig>,
    plugin: Arc<dyn FrameworkPlugin>,

    static_session_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl DbgManager {
    async fn init_discovery(&self) -> Result<()> {
        let config = self.config.as_ref();
        let plugin = self.plugin.as_ref();
        let (producer_tx, producer_rx) =
            flume::bounded::<crate::discovery::ServiceInfo>(SERVICE_DISCOVERY_QUEUE_CAPACITY);

        match plugin.service_discovery_mode(config) {
            ServiceDiscoveryMode::MessageBroker => {
                let discovery = config
                    .service_discovery
                    .as_ref()
                    .ok_or_else(|| anyhow!("message-broker discovery requires configuration"))?;
                let managed_broker: Option<Box<dyn MessageBroker>> =
                    match discovery.broker.managed.as_ref() {
                        Some(managed) => Some(match managed.broker_type {
                            common::config::BrokerType::Mosquitto => {
                                Box::new(MosquittoBroker::new())
                            }
                            common::config::BrokerType::Emqx => Box::new(EMQXBroker::new()),
                            common::config::BrokerType::Unknown => {
                                bail!("managed broker type must be mosquitto or emqx")
                            }
                        }),
                        None => None,
                    };

                let mut producer = crate::discovery::mqtt_producer::MqttProducer::new(
                    managed_broker,
                    Arc::clone(&self.config),
                );
                let start_result = producer.start_producing(producer_tx.clone()).await;
                self.discovery.lock().await.replace(DiscoveryRuntime::new(
                    Box::new(producer),
                    producer_rx,
                    None,
                    self.factory.clone(),
                    Arc::clone(&self.supervisor),
                ));
                start_result.context("failed to start MQTT discovery producer")?;
                info!("MQTT discovery producer started successfully");
            }
            ServiceDiscoveryMode::Kubernetes => {
                let service_weaver = config
                    .service_discovery
                    .as_ref()
                    .and_then(|discovery| discovery.service_weaver_conf.as_ref())
                    .ok_or_else(|| {
                        anyhow!("Kubernetes discovery requires Service Weaver configuration")
                    })?;
                let (exited_sender, _exited) = tokio::sync::watch::channel(false);
                let jump_client_config = RusshClientConfig {
                    nodelay: true,
                    ..RusshClientConfig::default()
                };
                let mut jump_host = russh::client::connect(
                    Arc::new(jump_client_config),
                    (
                        service_weaver.jump_client_host.clone(),
                        service_weaver.jump_client_port,
                    ),
                    crate::connection::ssh_client_channel::SSHProxyClientHandler(exited_sender),
                )
                .await
                .context("failed to connect to the Kubernetes SSH jump host")?;

                match jump_host
                    .authenticate_password(
                        service_weaver.jump_client_user.clone(),
                        service_weaver.jump_client_password.clone(),
                    )
                    .await
                    .context("failed to authenticate with the Kubernetes SSH jump host")?
                {
                    russh::client::AuthResult::Success => {
                        debug!("jump-host password authentication succeeded");
                    }
                    russh::client::AuthResult::Failure {
                        remaining_methods, ..
                    } => {
                        bail!(
                            "jump-host password authentication failed; remaining methods: {:?}",
                            remaining_methods
                        );
                    }
                }

                // OpenSSH enables TCP_NODELAY when a session command starts,
                // but not for connections that only use direct-tcpip channels.
                // Run a no-op command once so small forwarded replies are not
                // held behind the peer's delayed ACK timer.
                let mut latency_channel = jump_host
                    .channel_open_session()
                    .await
                    .context("failed to open the Kubernetes SSH latency warm-up channel")?;
                latency_channel
                    .exec(true, "true")
                    .await
                    .context("failed to prime the Kubernetes SSH jump host")?;
                while latency_channel.wait().await.is_some() {}
                let jump_host = Arc::new(jump_host);
                let mut producer = crate::discovery::k8s_producer::K8sProducer::new(
                    config.clone(),
                    service_weaver.service_name.clone(),
                );
                let start_result = producer.start_producing(producer_tx.clone()).await;
                self.discovery.lock().await.replace(DiscoveryRuntime::new(
                    Box::new(producer),
                    producer_rx,
                    Some(jump_host),
                    self.factory.clone(),
                    Arc::clone(&self.supervisor),
                ));
                start_result.context("failed to start Kubernetes discovery producer")?;
            }
            ServiceDiscoveryMode::None => {}
        }
        Ok(())
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
    pub(crate) async fn new(services: &AppContext) -> Result<Arc<Self>> {
        let config = Arc::clone(services.config());
        let plugin = Arc::clone(services.plugin());

        let proclet_ctrl = if plugin.supports_migration(config.as_ref()) {
            debug!("Migration support is ENABLED, initializing proxy proclet controller.");
            Some(
                ProcletCtrlClient::try_connect_default()
                    .await
                    .context("failed to connect to proclet controller")?,
            )
        } else {
            debug!("[Migration SUPPORT]: DISABLED. SKIP initializing proxy proclet controller.");
            None
        };

        let supervisor = SessionSupervisor::new(
            Arc::clone(&config),
            Arc::clone(&plugin),
            Arc::clone(services.runtime_model()),
            Arc::clone(services.command_router()),
            Arc::clone(services.notification_manager()),
            Arc::clone(services.group_operations()),
            Arc::clone(services.source_resolver()),
        );
        let factory = SessionFactory::new(
            Arc::clone(&config),
            Arc::clone(services.backend()),
            Arc::clone(&plugin),
            Arc::clone(services.event_reducer()),
        );

        Ok(Arc::new(DbgManager {
            supervisor,
            factory,
            discovery: Mutex::new(None),
            proclet_ctrl,
            config,
            plugin,
            static_session_handles: Mutex::new(Vec::new()),
        }))
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.supervisor.start().await?;
        if self.config.service_discovery.is_some() {
            info!("[Service Discovery]: ENABLED. INIT service discovery...");
            self.init_discovery().await?;
        } else {
            info!("[Service Discovery]: DISABLED. SKIP service discovery initialization.");
        }
        if let Some(discovery) = &mut *self.discovery.lock().await {
            discovery.start()?;
            debug!("DbgManager is now listening for discovered services.");
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

        self.supervisor.shutdown().await;
        debug!("[DbgManager]: Cleanup complete.");
    }
}

impl DbgManager {
    pub async fn query_proclet(&self, proclet_id: u64) -> Result<QueryProcletResp> {
        if let Some(ctrl) = &self.proclet_ctrl {
            return ctrl.query_proclet(proclet_id).await;
        }
        bail!("Proclet controller not available.")
    }
}
