use anyhow::{anyhow, bail, Result};
use flume::Receiver;
use russh::client::Config as RusshClientConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::discovery::broker::{EMQXBroker, MessageBroker, MosquittoBroker};
use crate::feature::proclet_ctrl::{ProcletCtrlClient, QueryProcletResp};
use crate::plugin::{get_framework_plugin, ServiceDiscoveryMode};
use crate::session::{factory::SessionFactory, supervisor::SessionSupervisor};
use crate::{
    common::{
        self,
        config::{Config as DDBConfig, StaticSessionConfig},
    },
    discovery::DiscoveryMessageProducer,
};

const SERVICE_DISCOVERY_QUEUE_CAPACITY: usize = 256;

pub struct ServiceDiscover {
    /// The service discovery producer that will send `ServiceInfo` events.
    ///
    /// Producer (and ServiceDiscover) lifecycle should be managed by the `DbgManager`.
    pub producer: Box<dyn DiscoveryMessageProducer>,

    /// The channel receiver for receiving `ServiceInfo` events.
    pub rx: Receiver<crate::discovery::ServiceInfo>,

    pub handle: Option<JoinHandle<()>>,

    pub proxy_tunnel: Option<
        Arc<russh::client::Handle<crate::connection::ssh_client_channel::SSHProxyClientHandler>>,
    >,
}

impl ServiceDiscover {
    pub fn new(
        producer: Box<dyn DiscoveryMessageProducer>,
        rx: Receiver<crate::discovery::ServiceInfo>,
        proxy_tunnel: Option<
            Arc<
                russh::client::Handle<crate::connection::ssh_client_channel::SSHProxyClientHandler>,
            >,
        >,
    ) -> Self {
        ServiceDiscover {
            producer,
            rx,
            handle: None,
            proxy_tunnel,
        }
    }

    // async fn notify_new_session()

    /// Handles creation of a new debug session for a discovered service.
    /// Called by the consumer loop that reads from a unified channel of `ServiceInfo<T>`.
    async fn prepare_new_session(
        manager: Arc<DbgManager>,
        info: crate::discovery::ServiceInfo,
        proxy_tunnel: Option<crate::dbg_ctrl::ProxyTunnel>,
    ) {
        let process = match manager.factory.create_discovered(info, proxy_tunnel) {
            Ok(process) => process,
            Err(error) => {
                error!(?error, "failed to construct discovered session");
                return;
            }
        };
        if let Err(error) = manager.supervisor.admit(process).await {
            error!(?error, "failed to admit discovered session");
        }
    }

    pub fn start(&mut self, manager: std::sync::Weak<DbgManager>) {
        let rx = self.rx.clone();
        let proxy_tunnel = self.proxy_tunnel.clone();
        let handle = tokio::spawn(async move {
            while let Ok(info) = rx.recv_async().await {
                let Some(manager) = manager.upgrade() else {
                    break;
                };
                // For each discovered service, create a new debug session.
                debug!("Received service info: {:?}", info);
                Self::prepare_new_session(manager, info, proxy_tunnel.clone()).await;
            }
        });
        self.handle = Some(handle);
    }

    pub async fn shutdown(&mut self) {
        self.handle.take().map(|h| h.abort());
        self.rx.drain();
        self.producer.stop_producing().await.unwrap();
    }
}

/// The manager that can handle multiple producers, each sending discovered services.
pub struct DbgManager {
    supervisor: Arc<SessionSupervisor>,
    factory: SessionFactory<'static>,

    /// We keep the producers in a vector (each implements `DiscoveryMessageProducer<T>`).
    // producers: Mutex<Vec<Box<dyn crate::discovery::DiscoveryMessageProducer>>>,

    // ServiceDiscover, which receives the discovered services information.
    sd: Mutex<Option<ServiceDiscover>>,

    // This should be non-null if the framework is Nu/Quicksand and migration support is enabled.
    proclet_ctrl: Option<ProcletCtrlClient>,

    config: &'static DDBConfig,

    static_session_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl DbgManager {
    async fn init_sd(&self) -> Result<()> {
        let config = self.config;
        let plugin = get_framework_plugin();
        // Discovery is bursty, but session creation is comparatively expensive.
        // Bound the handoff so producers slow down instead of accumulating an
        // unbounded number of pending sessions.
        let (producer_tx, producer_rx) =
            flume::bounded::<crate::discovery::ServiceInfo>(SERVICE_DISCOVERY_QUEUE_CAPACITY);
        match plugin.service_discovery_mode(config) {
            ServiceDiscoveryMode::MessageBroker => {
                let sd = config
                    .service_discovery
                    .as_ref()
                    .ok_or(anyhow!("ERROR: broker is not specified when it is needed."))?;

                if let Some(managed_broker_conf) = sd.broker.managed.as_ref() {
                    let b: Box<dyn MessageBroker> = match managed_broker_conf.broker_type {
                        common::config::BrokerType::Mosquitto => Box::new(MosquittoBroker::new()),
                        common::config::BrokerType::Emqx => Box::new(EMQXBroker::new()),
                        _ => {
                            panic!("Broker type not supported yet.");
                        }
                    };
                    let mut mqtt_producer =
                        crate::discovery::mqtt_producer::MqttProducer::new(Some(b), &config);
                    let producer_tx_clone = producer_tx.clone();

                    // Note: start_producing may fail if the broker is offline.
                    // We delay the error checking as we want to have `sd` to be initialized first.
                    // So that it can be cleaned up properly in case of failure (via `cleanup`).
                    let result = mqtt_producer.start_producing(producer_tx_clone).await;

                    self.sd.lock().await.replace(ServiceDiscover::new(
                        Box::new(mqtt_producer),
                        producer_rx,
                        None,
                    ));

                    match result {
                        Ok(_) => {
                            info!("MQTT broker/producer started successfully.");
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
            }
            ServiceDiscoveryMode::Kubernetes => {
                let swc = config
                    .service_discovery
                    .as_ref()
                    .and_then(|sd| sd.service_weaver_conf.as_ref())
                    .expect("Service weaver config missing for service weaver auto discovery.");
                let (exited_sender, _exited) = tokio::sync::watch::channel(false);
                let jump_client_config = RusshClientConfig {
                    nodelay: true,
                    ..RusshClientConfig::default()
                };
                let mut jump_host_session = russh::client::connect(
                    Arc::new(jump_client_config),
                    (swc.jump_client_host.clone(), swc.jump_client_port),
                    crate::connection::ssh_client_channel::SSHProxyClientHandler(exited_sender),
                )
                .await
                .unwrap();
                match jump_host_session
                    .authenticate_password(
                        swc.jump_client_user.clone(),
                        swc.jump_client_password.clone(),
                    )
                    .await
                {
                    Ok(auth_result) => match auth_result {
                        russh::client::AuthResult::Success => {
                            debug!("Password authentication successful");
                        }
                        russh::client::AuthResult::Failure {
                            remaining_methods, ..
                        } => {
                            panic!(
                                "Password authentication failed. Available methods: {:?}",
                                remaining_methods
                            );
                        }
                    },
                    Err(e) => {
                        panic!("Authentication error: {:?}", e);
                    }
                }

                // OpenSSH enables TCP_NODELAY when a session command starts,
                // but not for connections that only use direct-tcpip channels.
                // Run a no-op command once so small forwarded replies are not
                // held behind the peer's delayed ACK timer.
                let mut latency_channel = jump_host_session.channel_open_session().await.unwrap();
                latency_channel.exec(true, "true").await.unwrap();
                while latency_channel.wait().await.is_some() {}

                let jump_host_session = Arc::new(jump_host_session);
                let mut serviceweaver_producer = crate::discovery::k8s_producer::K8sProducer::new(
                    config.clone(),
                    swc.service_name.clone(),
                );
                let producer_tx_clone = producer_tx.clone();
                serviceweaver_producer
                    .start_producing(producer_tx_clone)
                    .await
                    .unwrap();

                self.sd.lock().await.replace(ServiceDiscover::new(
                    Box::new(serviceweaver_producer),
                    producer_rx,
                    Some(jump_host_session),
                ));
            }
            ServiceDiscoveryMode::None => {}
        }
        Ok(())
    }

    async fn start_static_session(&self, session: StaticSessionConfig) -> Result<()> {
        if session.start_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(session.start_delay_ms)).await;
        }

        let process = self.factory.create_static(session)?;
        self.supervisor.admit(process).await?;
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
    pub async fn new() -> Arc<Self> {
        Self::new_with_config(DDBConfig::global()).await
    }

    pub async fn new_with_config(config: &'static DDBConfig) -> Arc<Self> {
        let plugin = get_framework_plugin();

        let proclet_ctrl = if plugin.supports_migration(config) {
            debug!("Migration support is ENABLED, initializing proxy proclet controller.");
            Some(
                ProcletCtrlClient::try_connect_default()
                    .await
                    .expect("Failed to connect to proclet controller"),
            )
        } else {
            debug!("[Migration SUPPORT]: DISABLED. SKIP initializing proxy proclet controller.");
            None
        };

        Arc::new(DbgManager {
            supervisor: SessionSupervisor::new(config),
            factory: SessionFactory::new(config),
            sd: Mutex::new(None),
            proclet_ctrl,
            config,
            static_session_handles: Mutex::new(Vec::new()),
        })
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.supervisor.start().await?;
        if self.config.service_discovery.is_some() {
            info!("[Service Discovery]: ENABLED. INIT service discovery...");
            self.init_sd().await?;
        } else {
            info!("[Service Discovery]: DISABLED. SKIP service discovery initialization.");
        }
        if let Some(sd) = &mut *self.sd.lock().await {
            sd.start(Arc::downgrade(self));
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

        if let Some(sd) = &mut *self.sd.lock().await {
            debug!("Shutting down ServiceDiscovery...");
            sd.shutdown().await;
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
