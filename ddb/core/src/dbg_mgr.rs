use anyhow::{anyhow, bail, Result};
use dashmap::DashMap;
use flume::Receiver;
use futures::future::join_all;
use russh::client::Config as RusshClientConfig;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, error, info, trace};

use crate::dbg_ctrl::{build_transport, TransportSpec};
use crate::discovery::broker::{EMQXBroker, MessageBroker, MosquittoBroker};
use crate::discovery::discovery_message_producer::ServiceMeta;
use crate::feature::proclet_ctrl::{ProcletCtrlClient, QueryProcletResp};
use crate::notification::{get_notif_mgr, Notification, NotificationPayload};
use crate::plugin::{get_framework_plugin, ServiceDiscoveryMode};
use crate::session::lifecycle::{self, SessionLifecycleHandle, SessionTermination};
use crate::shutdown::get_shutdown_ctrl;
use crate::state::{get_caladan_ip_from_user_data, get_proclet_mgr};
use crate::{
    common::{
        self,
        config::{
            Config as DDBConfig, DebuggerBackendKind, StaticSessionConfig, StaticSessionStartMode,
        },
    },
    discovery::DiscoveryMessageProducer,
};

const SERVICE_DISCOVERY_QUEUE_CAPACITY: usize = 256;

type DebuggerSessionRef = Arc<Mutex<crate::session::DbgSession>>;

/// For convenience, a type alias to store sessions in a DashMap (sid -> session).
type SessionsRef = Arc<DashMap<u64, DebuggerSessionRef>>;

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
        proxy_tunnel: Option<
            Arc<
                russh::client::Handle<crate::connection::ssh_client_channel::SSHProxyClientHandler>,
            >,
        >,
    ) {
        let service_meta = ServiceMeta::from(&info);
        let pid = info.pid;
        let caladan_ip = get_caladan_ip_from_user_data(&service_meta.user_data);
        let request = match crate::session::SessionRequestBuilder::from_config(manager.config)
            .tag(info.tag)
            .mode(crate::session::SessionMode::Remote(
                crate::session::SessionStart::Attach(pid),
            ))
            .transport(info.transport)
            .service_meta(service_meta)
            .caladan_ip(caladan_ip)
            .build()
        {
            Ok(request) => request,
            Err(error) => {
                error!(?error, "failed to validate discovered session request");
                return;
            }
        };

        let transport = match build_transport(&request.transport, proxy_tunnel) {
            Ok(transport) => transport,
            Err(error) => {
                error!(?error, "failed to construct debugger transport");
                return;
            }
        };
        let dbg_session = crate::session::DbgSession::new(request, transport);
        if let Err(error) = manager.start_session(dbg_session).await {
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

    lifecycle: SessionLifecycleHandle,
    lifecycle_events: Mutex<Option<mpsc::UnboundedReceiver<SessionTermination>>>,
    lifecycle_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    lifecycle_task: Mutex<Option<JoinHandle<()>>>,
}

impl DbgManager {
    async fn start_lifecycle_supervisor(self: &Arc<Self>) -> Result<()> {
        let mut events = self.lifecycle_events.lock().await;
        let mut events = events
            .take()
            .ok_or_else(|| anyhow!("debugger lifecycle supervisor is already started"))?;
        let (shutdown, mut shutdown_requested) = oneshot::channel();
        self.lifecycle_shutdown.lock().await.replace(shutdown);
        let manager = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let mut cleanups = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_requested => break,
                    termination = events.recv() => {
                        let Some(termination) = termination else {
                            break;
                        };
                        let Some(manager) = manager.upgrade() else {
                            break;
                        };
                        cleanups.spawn(async move {
                            manager.finish_session(termination).await;
                        });
                    }
                    Some(result) = cleanups.join_next(), if !cleanups.is_empty() => {
                        if let Err(error) = result {
                            error!(?error, "session lifecycle cleanup task failed");
                        }
                    }
                }
            }
            while let Some(result) = cleanups.join_next().await {
                if let Err(error) = result {
                    error!(?error, "session lifecycle cleanup task failed");
                }
            }
        });
        self.lifecycle_task.lock().await.replace(task);
        Ok(())
    }

    async fn start_session(&self, dbg_session: crate::session::DbgSession) -> Result<u64> {
        let sid = dbg_session.sid;
        let caladan_ip = dbg_session.request.caladan_ip;
        let termination = self.lifecycle.bind(sid);
        let session = Arc::new(Mutex::new(dbg_session));
        self.sessions.insert(sid, Arc::clone(&session));

        let start_result = {
            let mut session = session.lock().await;
            match session.start(termination.clone()).await {
                Ok(_) => session.post_start().await,
                Err(error) => Err(error),
            }
        };

        match start_result {
            Ok(_) if !termination.termination_requested() && self.sessions.contains_key(&sid) => {
                if get_framework_plugin().should_register_caladan_ip(self.config) {
                    if let Some(caladan_ip) = caladan_ip {
                        get_proclet_mgr().register_owner_session(caladan_ip, sid);
                    }
                }

                debug!(sid, "session started successfully");
                let notification = Notification::new(NotificationPayload::SessionListChanged);
                get_notif_mgr().broadcast(notification).await;
                Ok(sid)
            }
            Ok(_) => Err(anyhow!(
                "session {} terminated before startup completed",
                sid
            )),
            Err(error) => {
                self.sessions.remove(&sid);
                if let Err(cleanup_error) = session.lock().await.cleanup().await {
                    error!(sid, ?cleanup_error, "failed to roll back session startup");
                }
                Err(error)
            }
        }
    }

    async fn finish_session(&self, termination: SessionTermination) {
        debug!(
            sid = termination.sid,
            cause = ?termination.cause,
            "session termination requested"
        );
        self.remove_session(termination.sid).await;
    }

    async fn remove_session(&self, sid: u64) {
        let Some((_, session)) = self.sessions.remove(&sid) else {
            trace!(sid, "ignoring termination for an unregistered session");
            return;
        };

        if let Err(error) = session.lock().await.cleanup().await {
            error!(sid, ?error, "session cleanup failed");
        }

        let notification = Notification::new(NotificationPayload::SessionListChanged);
        get_notif_mgr().broadcast(notification).await;

        if self.config.conf.auto_shutdown {
            if self.sessions.is_empty() {
                debug!("No more sessions in DbgManager. Possibly shutting down…");
                get_shutdown_ctrl().trigger_once(crate::shutdown::ShutdownCause::NoSessions);
            }
        }
    }

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

        let service_meta = ServiceMeta::new(
            session.ip,
            session.tag.clone(),
            session.pid,
            session.hash.clone(),
            session.alias.clone(),
            None,
        );

        let mut builder = crate::session::SessionRequestBuilder::from_config(self.config)
            .tag(session.tag.clone())
            .stop_at_entry(session.stop_at_entry)
            .service_meta(service_meta);

        builder = match self.config.conf.debugger.backend {
            DebuggerBackendKind::Mock => builder
                .mode(crate::session::SessionMode::Remote(
                    crate::session::SessionStart::Attach(session.pid),
                ))
                .transport(TransportSpec::Mock {
                    config: session.mock.clone(),
                    pid: session.pid,
                }),
            DebuggerBackendKind::Gdb => {
                let mode = match session.start_mode {
                    StaticSessionStartMode::Attach => {
                        if session.pid == 0 {
                            bail!("static attach sessions require a non-zero pid");
                        }
                        crate::session::SessionMode::Local(crate::session::SessionStart::Attach(
                            session.pid,
                        ))
                    }
                    StaticSessionStartMode::Binary => {
                        if session.binary_path.trim().is_empty() {
                            bail!("static binary sessions require binary_path to be set");
                        }
                        crate::session::SessionMode::Local(crate::session::SessionStart::Binary {
                            path: session.binary_path.clone(),
                            args: session.binary_args.clone(),
                        })
                    }
                };
                builder.mode(mode).transport(TransportSpec::Local)
            }
            DebuggerBackendKind::Unknown => bail!("Unsupported debugger backend configured."),
        };

        let request = builder.build()?;
        let transport = build_transport(&request.transport, None)?;
        let dbg_session = crate::session::DbgSession::new(request, transport);
        self.start_session(dbg_session).await?;
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
        let sessions: SessionsRef = Arc::new(DashMap::new());
        let (lifecycle, lifecycle_events) = lifecycle::channel();
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
            sessions,
            sd: Mutex::new(None),
            proclet_ctrl,
            config,
            static_session_handles: Mutex::new(Vec::new()),
            lifecycle,
            lifecycle_events: Mutex::new(Some(lifecycle_events)),
            lifecycle_shutdown: Mutex::new(None),
            lifecycle_task: Mutex::new(None),
        })
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.start_lifecycle_supervisor().await?;
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

        if let Some(shutdown) = self.lifecycle_shutdown.lock().await.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.lifecycle_task.lock().await.take() {
            let _ = task.await;
        }

        let keys: Vec<_> = self.sessions.iter().map(|e| *e.key()).collect();
        let mut sessions = Vec::with_capacity(keys.len());
        for sid in keys {
            if let Some((_, session)) = self.sessions.remove(&sid) {
                sessions.push((sid, session));
            }
        }

        join_all(sessions.into_iter().map(|(sid, session)| async move {
            if let Err(error) = session.lock().await.cleanup().await {
                error!(
                    sid,
                    ?error,
                    "session cleanup failed during manager shutdown"
                );
            }
        }))
        .await;
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
