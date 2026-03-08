use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use flume::Receiver;
use futures::future::join_all;
use russh::client::Config as RusshClientConfig;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::discovery::broker::{EMQXBroker, MessageBroker, MosquittoBroker};
use crate::discovery::discovery_message_producer::{ServiceInfo, ServiceMeta};
use crate::feature::proclet_ctrl::{ProcletCtrlClient, QueryProcletResp};
use crate::notification::{get_notif_mgr, Notification, NotificationPayload};
use crate::plugin::{get_framework_plugin, ServiceDiscoveryMode};
use crate::shutdown::get_shutdown_ctrl;
use crate::state::{get_caladan_ip_from_user_data, get_proclet_mgr, get_state_mgr};
use crate::{
    common::{
        self,
        config::{Config as DDBConfig, DebuggerBackendKind, StaticSessionConfig},
    },
    discovery::DiscoveryMessageProducer,
};

#[async_trait]
pub trait DbgManagable {
    async fn new() -> Self
    where
        Self: Sized,
    {
        let gconf = DDBConfig::global();
        Self::new_with_config(gconf).await
    }

    async fn new_with_config(config: &'static DDBConfig) -> Self;
    async fn start(&self) -> Result<()>;
    async fn cleanup(&self);
}

pub type DebuggerSessionRef = crate::session::DbgSession;

/// For convenience, a type alias to store sessions in a DashMap (sid -> session).
pub type SessionsRef = Arc<DashMap<u64, DebuggerSessionRef>>;

pub struct ServiceDiscover {
    /// The service discovery producer that will send `ServiceInfo` events.
    ///
    /// Producer (and ServiceDiscover) lifecycle should be managed by the `DbgManager`.
    pub producer: Box<dyn DiscoveryMessageProducer>,

    /// The channel receiver for receiving `ServiceInfo` events.
    pub rx: Receiver<crate::discovery::ServiceInfo>,

    pub handle: Option<JoinHandle<()>>,
}

impl ServiceDiscover {
    pub fn new(
        producer: Box<dyn DiscoveryMessageProducer>,
        rx: Receiver<crate::discovery::ServiceInfo>,
    ) -> Self {
        ServiceDiscover {
            producer,
            rx,
            handle: None,
        }
    }

    // async fn notify_new_session()

    /// Handles creation of a new debug session for a discovered service.
    /// Called by the consumer loop that reads from a unified channel of `ServiceInfo<T>`.
    async fn prepare_new_session(sessions: SessionsRef, info: crate::discovery::ServiceInfo) {
        let service_meta = ServiceMeta::from(&info);
        let hostname = info.ip;
        let pid = info.pid;
        let tag_str = info.tag;
        // if no such field is provided, it will be None.
        // so it is ok to leave it here.
        let caladan_ip = get_caladan_ip_from_user_data(&service_meta.user_data);

        let s_cfg = crate::session::DbgSessionCfgBuilder::new()
            .tag(tag_str)
            // Possibly do something more direct with the `info.controller`
            // if your code needs to embed or pass it in.
            .ssh_cred(hostname) // for example
            .mode(crate::session::DbgMode::REMOTE(
                crate::session::DbgStartMode::ATTACH(pid),
            ))
            .add_prerun_debugger_cmd(
                crate::dbg_cmd::GdbCmd::SetOption(crate::dbg_cmd::GdbOption::MiAsync(true)).into(),
            )
            .with_debugger_controller(info.ssh_controller)
            .with_service_meta(service_meta)
            .build();

        // Build the session
        let mut dbg_session = crate::session::DbgSession::new(s_cfg);
        let new_sid = dbg_session.sid;

        // Attempt to start the session
        match dbg_session.start().await {
            Ok(input_tx) => {
                // Register with your command router or anywhere else
                crate::cmd_flow::get_router().add_session(new_sid, input_tx);

                // Register with proclet manager if needed.
                // so that we have the mapping between caladan ip and session id.
                let g_cfg = DDBConfig::global();
                if get_framework_plugin().should_register_caladan_ip(g_cfg) {
                    if let Some(caladan_ip) = caladan_ip {
                        get_proclet_mgr().register_owner_session(caladan_ip, new_sid);
                    }
                }
                match dbg_session.post_start().await {
                    Ok(_) => {
                        // Put it in the manager's DashMap
                        sessions.insert(new_sid, dbg_session);
                        debug!("Session {} started successfully.", new_sid);
                    }
                    Err(e) => {
                        error!("Post-start actions for session {} failed: {:?}", new_sid, e);
                    }
                }
                let notification = Notification::new(NotificationPayload::SessionListChanged);
                get_notif_mgr().broadcast(notification).await;
            }
            Err(e) => {
                // If failure, remove from global state
                error!("Failed to start session {}: {:?}", new_sid, e);
                // best-effort clean up for a broken session.
                let _ = dbg_session.cleanup().await;
                get_state_mgr().remove_session(new_sid).await;
            }
        }
    }

    pub fn start(&mut self, sessions: SessionsRef) {
        let rx = self.rx.clone();
        let handle = tokio::spawn(async move {
            while let Ok(info) = rx.recv_async().await {
                // For each discovered service, create a new debug session.
                debug!("Received service info: {:?}", info);
                Self::prepare_new_session(sessions.clone(), info).await;
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
    /// All active GDB sessions (keyed by session id).
    sessions: SessionsRef,

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
    /// Removes (and cleans up) a given session, for external calls or internal use.
    pub async fn remove_session(&self, sid: u64) {
        if let Some((_, mut s)) = self.sessions.remove(&sid) {
            let _ = s.cleanup().await;
        }

        let notification = Notification::new(NotificationPayload::SessionListChanged);
        get_notif_mgr().broadcast(notification).await;

        let config = DDBConfig::global();
        if config.conf.auto_shutdown {
            if self.sessions.is_empty() {
                debug!("No more sessions in DbgManager. Possibly shutting down…");
                get_shutdown_ctrl().trigger_once(crate::shutdown::ShutdownCause::NoSessions);
            }
        }
    }

    async fn init_sd(&self) -> Result<()> {
        let config = self.config;
        let plugin = get_framework_plugin();
        let (producer_tx, producer_rx) = flume::unbounded::<crate::discovery::ServiceInfo>();
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

                    self.sd
                        .lock()
                        .await
                        .replace(ServiceDiscover::new(Box::new(mqtt_producer), producer_rx));

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

                let mut serviceweaver_producer = crate::discovery::k8s_producer::K8sProducer::new(
                    config.clone(),
                    Arc::new(jump_host_session),
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
                ));
            }
            ServiceDiscoveryMode::None => {}
        }
        Ok(())
    }

    fn static_service_info(
        config: &'static DDBConfig,
        session: StaticSessionConfig,
    ) -> Result<ServiceInfo> {
        let controller = match config.conf.debugger.backend {
            DebuggerBackendKind::Mock => Box::new(crate::dbg_ctrl::MockAttachController::new(
                session.mock.clone(),
                session.pid,
            )) as crate::dbg_ctrl::DbgController,
            DebuggerBackendKind::Gdb => {
                let ssh_cred = crate::connection::ssh_client::SSHCred::new(
                    &session.ip.to_string(),
                    config.ssh.port,
                    config.ssh.user.as_str(),
                    None,
                );
                Box::new(crate::dbg_ctrl::SSHAttachController::new(ssh_cred))
                    as crate::dbg_ctrl::DbgController
            }
            DebuggerBackendKind::Unknown => bail!("Unsupported debugger backend configured."),
        };

        Ok(ServiceInfo::new(
            session.ip,
            session.tag,
            session.pid,
            session.hash,
            session.alias,
            controller,
            None,
        ))
    }

    async fn start_static_session(
        sessions: SessionsRef,
        config: &'static DDBConfig,
        session: StaticSessionConfig,
    ) -> Result<()> {
        if session.start_delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(session.start_delay_ms)).await;
        }
        let info = Self::static_service_info(config, session)?;
        ServiceDiscover::prepare_new_session(sessions, info).await;
        Ok(())
    }

    async fn init_static_sessions(&self) -> Result<()> {
        let mut delayed_handles = self.static_session_handles.lock().await;
        delayed_handles.clear();

        for session in self.config.static_sessions.clone() {
            if session.start_delay_ms == 0 {
                Self::start_static_session(self.sessions.clone(), self.config, session).await?;
            } else {
                let sessions = self.sessions.clone();
                let config = self.config;
                delayed_handles.push(tokio::spawn(async move {
                    if let Err(error) = Self::start_static_session(sessions, config, session).await
                    {
                        error!("Failed to start delayed static session: {:?}", error);
                    }
                }));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl DbgManagable for DbgManager {
    async fn new_with_config(config: &'static DDBConfig) -> Self {
        let sessions: SessionsRef = Arc::new(DashMap::new());
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

        DbgManager {
            sessions: sessions.clone(),
            sd: Mutex::new(None),
            proclet_ctrl,
            config,
            static_session_handles: Mutex::new(Vec::new()),
        }
    }

    async fn start(&self) -> Result<()> {
        if let Some(_) = DDBConfig::global().service_discovery {
            info!("[Service Discovery]: ENABLED. INIT service discovery...");
            self.init_sd().await?;
        } else {
            info!("[Service Discovery]: DISABLED. SKIP service discovery initialization.");
        }
        if let Some(sd) = &mut *self.sd.lock().await {
            sd.start(self.sessions.clone());
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

    async fn cleanup(&self) {
        let mut handles = self.static_session_handles.lock().await;
        for handle in handles.drain(..) {
            handle.abort();
        }

        // 1) Shutdown the service discovery if it exists
        if let Some(sd) = &mut *self.sd.lock().await {
            debug!("Shutting down ServiceDiscovery...");
            sd.shutdown().await;
        }

        // 2) Clean up all existing sessions
        let keys: Vec<_> = self.sessions.iter().map(|e| *e.key()).collect();
        let mut tasks = vec![];
        for sid in keys {
            if let Some((_, mut session)) = self.sessions.remove(&sid) {
                crate::cmd_flow::get_router().remove_session(sid);
                tasks.push(tokio::spawn(async move {
                    let _ = session.cleanup().await;
                    crate::state::STATES.remove_session(sid).await;
                }));
            }
        }

        join_all(tasks).await;
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
