use anyhow::{bail, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use flume::Receiver;
use futures::future::join_all;
use russh::client::Config;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error};

use crate::discovery::broker::{EMQXBroker, MessageBroker, MosquittoBroker};
use crate::discovery::discovery_message_producer::ServiceMeta;
use crate::feature::proclet_ctrl::{ProcletCtrlClient, ProcletCtrlCmdResp, QueryProcletResp};
use crate::state::{get_caladan_ip_from_user_data, get_proclet_mgr};
use crate::{
    common::{self, config::Framework},
    discovery::DiscoveryMessageProducer,
};

/// Trait for something that can be started/stopped (like your old `DbgManagable`).
#[async_trait]
pub trait DbgManagable {
    /// Create a new manager using the global config.
    async fn new() -> Self
    where
        Self: Sized,
    {
        let gconf = crate::common::config::Config::global();
        Self::new_with_config(gconf).await
    }

    /// Create a new manager with a specific config reference.
    async fn new_with_config(config: &crate::common::config::Config) -> Self;

    /// Start the manager (start producers, spawn consumer tasks, etc.).
    async fn start(&self);

    /// Clean up everything (producers + sessions).
    async fn cleanup(&self);
}

/// A reference to one debug session with a particular `DbgControllable`.
pub type GdbSessionRef = crate::session::DbgSession;

/// For convenience, a type alias to store sessions in a DashMap (sid -> session).
pub type SessionsRef = Arc<DashMap<u64, GdbSessionRef>>;

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
    /// Create a new `ServiceDiscover` instance with a producer and receiver.
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
            .add_prerun_gdb_cmd(
                crate::dbg_cmd::GdbCmd::SetOption(crate::dbg_cmd::GdbOption::MiAsync(true)).into(),
            )
            .add_gdb_controller(info.ssh_controller)
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
                let g_cfg = crate::common::config::Config::global();
                match g_cfg.framework {
                    Framework::Nu | Framework::Quicksand => {
                        if g_cfg.conf.support_migration {
                            if let Some(caladan_ip) = caladan_ip {
                                get_proclet_mgr().register_caladan_ip(caladan_ip, new_sid);
                            }
                        }
                    }
                    _ => {}
                }

                // Put it in the manager's DashMap
                sessions.insert(new_sid, dbg_session);

                debug!("Session {} started successfully.", new_sid);
            }
            Err(e) => {
                // If failure, remove from global state
                error!("Failed to start session {}: {:?}", new_sid, e);
                crate::state::STATES.remove_session(new_sid).await;
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
}

impl DbgManager {
    /// Removes (and cleans up) a given session, for external calls or internal use.
    pub async fn remove_session(&self, sid: u64) {
        if let Some((_, mut s)) = self.sessions.remove(&sid) {
            let _ = s.cleanup().await;
        }

        if self.sessions.is_empty() {
            debug!("No more sessions in GdbManager. Possibly shutting down…");
            crate::SHUTDOWN_SIGNAL.trigger();
        }
    }

    async fn init_sd(&mut self, config: &crate::common::config::Config) {
        let (producer_tx, producer_rx) = flume::unbounded::<crate::discovery::ServiceInfo>();
        match config.framework {
            Framework::Nu | Framework::GRPC => {
                let sd = config
                    .service_discovery
                    .as_ref()
                    .expect("Service discovery config missing for mqtt broker.");
                if let Some(broker_conf) = sd.broker.managed.as_ref() {
                    let b: Box<dyn MessageBroker> = match broker_conf.broker_type {
                        common::config::BrokerType::Mosquitto => {
                            // TODO: need some refactoring here to make sure Mosquitto works here.
                            Box::new(MosquittoBroker::new("".to_string()))
                        }
                        common::config::BrokerType::Emqx => Box::new(EMQXBroker::new()),
                        _ => {
                            panic!("Broker type not supported yet.");
                        }
                    };
                    let mut mqtt_producer =
                        crate::discovery::mqtt_producer::MqttProducer::new(Some(b), config.clone());
                    let producer_tx_clone = producer_tx.clone();

                    mqtt_producer
                        .start_producing(producer_tx_clone)
                        .await
                        .unwrap();

                    self.sd = Mutex::new(Some(ServiceDiscover::new(
                        Box::new(mqtt_producer),
                        producer_rx,
                    )));
                }
            }
            Framework::ServiceWeaverKube => {
                let swc = config
                    .service_weaver_conf
                    .as_ref()
                    .expect("Service weaver config missing for service weaver auto discovery.");
                let (exited_sender, _exited) = tokio::sync::watch::channel(false);
                let mut jump_host_session = russh::client::connect(
                    Arc::new(Config::default()),
                    (swc.jump_clinet_host.clone(), swc.jump_client_port),
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
                        russh::client::AuthResult::Failure { remaining_methods } => {
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

                self.sd = Mutex::new(Some(ServiceDiscover::new(
                    Box::new(serviceweaver_producer),
                    producer_rx,
                )));
            }
            _ => {
                panic!("Unsupported framework adapter for now.");
            }
        }
    }
}

#[async_trait]
impl DbgManagable for DbgManager {
    async fn new_with_config(config: &crate::common::config::Config) -> Self {
        let sessions: SessionsRef = Arc::new(DashMap::new());

        let proclet_ctrl = match config.framework {
            Framework::Nu | Framework::Quicksand => {
                // Initialize the proclet controller if needed
                debug!("Proclet controller is enabled.");
                if config.conf.support_migration {
                    Some(
                        ProcletCtrlClient::try_connect_default()
                            .await
                            .expect("Failed to connect to proclet controller"),
                    )
                } else {
                    None
                }
            }
            _ => None,
        };

        let mut dbg_mgr = DbgManager {
            sessions: sessions.clone(),
            sd: Mutex::new(None),
            proclet_ctrl,
        };
        dbg_mgr.init_sd(config).await;
        return dbg_mgr;
    }

    async fn start(&self) {
        if let Some(sd) = &mut *self.sd.lock().await {
            sd.start(self.sessions.clone());
        }
        debug!("GdbManager is now listening for discovered services.");
    }

    async fn cleanup(&self) {
        // 1) Shutdown the service discovery if it exists
        if let Some(sd) = &mut *self.sd.lock().await {
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

        // Wait for session cleanup
        join_all(tasks).await;

        debug!("GdbManager cleanup complete.");
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
