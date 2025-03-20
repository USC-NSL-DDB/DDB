// use async_trait::async_trait;
// use dashmap::DashMap;
// use futures::future::join_all;
// use std::sync::{Arc, Mutex};
// use tracing::debug;

// use crate::cmd_flow::get_router;
// use crate::common;
// use crate::dbg_cmd::{GdbCmd, GdbOption};
// use crate::dbg_ctrl::{DbgControllable, SSHAttachController};
// use crate::discovery::broker::{EMQXBroker, MessageBroker, MosquittoBroker};
// use crate::discovery::service_mgr::{ServiceMgr, ServiceMgrBuilder};
// // use crate::discovery::service_mgr::ServiceMgrBuilder;
// use crate::discovery::ServiceInfo;
// use crate::session::{DbgMode, DbgSessionCfgBuilder, DbgStartMode};
// use crate::state::STATES;

// // use super::discovery::service_mgr::ServiceMgr;
// use super::session::DbgSession;

// #[async_trait]
// pub trait DbgManagable {
//     fn new() -> Self
//     where
//         Self: Sized,
//     {
//         let gconf = common::config::Config::global();
//         Self::new_with_config(gconf)
//     }
//     fn new_with_config(config: &common::config::Config) -> Self;
//     async fn start(&self);
//     async fn cleanup(&self);
// }

// pub type DbgSessionRef<T> = DbgSession<T>;
// pub type SessionsRef<T> = Arc<DashMap<u64, DbgSessionRef<T>>>;

// pub struct DbgManager<T: DbgControllable> {
//     sessions: SessionsRef<T>,
//     service_mgr: Mutex<Option<ServiceMgr>>,
// }

// impl DbgManager<SSHAttachController> {
//     fn prepare_new_session(si: ServiceInfo, sessions: SessionsRef<SSHAttachController>) {
//         tokio::spawn(async move {
//             let hostname = si.ip;
//             let pid = si.pid;
//             let tag = format!("{}:-{}", hostname, pid);

//             let s_cfg: crate::session::DbgSessionConfig = DbgSessionCfgBuilder::new()
//                 .tag(tag)
//                 .ssh_cred(hostname)
//                 .mode(DbgMode::REMOTE(DbgStartMode::ATTACH(pid)))
//                 // ensure async mode is enabled by running it as prerun command
//                 .add_prerun_gdb_cmd(GdbCmd::SetOption(GdbOption::MiAsync(true)).into())
//                 .build();

//             debug!(
//                 "Preparing a new session: {:?}. session cfg: {:?}",
//                 si, s_cfg
//             );

//             let tag = s_cfg.tag.clone();
//             let mut dbg_s = DbgSession::new(s_cfg);
//             // Note: need to register the session before starting it.
//             // so that it has session meta entry to update if the start
//             // process needs to update the session state.
//             let new_sid = dbg_s.sid;
//             STATES
//                 .register_session(
//                     new_sid,
//                     tag.unwrap_or(format!("session-{}", new_sid)).as_str(),
//                 )
//                 .await;

//             let result = dbg_s.start().await;
//             match result {
//                 Ok(input_tx) => {
//                     // update router with input sender
//                     get_router().add_session(new_sid, input_tx);
//                     // update dbg manager
//                     sessions.insert(new_sid, dbg_s);
//                     debug!("Session started successfully.");
//                 }
//                 Err(e) => {
//                     // if failed to start, remove the session from the state manager.
//                     STATES.remove_session(new_sid).await;
//                     debug!("Failed to start session: {:?}", e);
//                 }
//             }
//         });
//     }

//     pub async fn remove_session(&self, sid: u64) {
//         if let Some((_, mut s)) = self.sessions.remove(&sid) {
//             // 1. remove from router
//             // 2. shutdown the connection
//             // 3. remove from state manager
//             get_router().remove_session(sid);
//             let _ = s.cleanup().await;
//             STATES.remove_session(sid).await;
//         }

//         if self.sessions.is_empty() {
//             debug!("No more sessions. Cleaning up.");
//             crate::SHUTDOWN_SIGNAL.trigger();
//         }
//     }
// }

// #[async_trait]
// impl DbgManagable for DbgManager<SSHAttachController> {
//     fn new() -> Self {
//         let gconf = common::config::Config::global();
//         Self::new_with_config(gconf)
//     }

//     fn new_with_config(config: &common::config::Config) -> Self {
//         let sessions = Arc::new(DashMap::new());

//         let svr_mgr = config.service_discovery.as_ref().map(|sd| {
//             let mut bdr = ServiceMgrBuilder::new()
//                 .broker_host(sd.broker.hostname.clone())
//                 .concurrency(10);

//             if let Some(broker_conf) = sd.broker.managed.as_ref() {
//                 let b: Box<dyn MessageBroker> = match broker_conf.broker_type {
//                     common::config::BrokerType::Mosquitto => {
//                         // TODO: need some refactoring here to make sure Mosquitto works here.
//                         Box::new(MosquittoBroker::new("".to_string()))
//                     }
//                     common::config::BrokerType::Emqx => Box::new(EMQXBroker::new()),
//                     _ => {
//                         panic!("Broker type not supported yet.");
//                     }
//                 };
//                 bdr = bdr.managed_broker(b).check_broker_online(true)
//             }

//             let ss = sessions.clone();
//             bdr.with_handler(move |si| {
//                 DbgManager::prepare_new_session(si, ss.clone());
//             })
//             .build()
//             .unwrap()
//         });

//         DbgManager {
//             sessions: sessions.clone(),
//             service_mgr: Mutex::new(svr_mgr),
//         }
//     }

//     async fn start(&self) {
//         // Don't support local start right now, so don't need to start sessions here.
//         // as sessions will be empty at this stage.

//         // Maybe implement this in the future.
//     }

//     async fn cleanup(&self) {
//         if let Some(s) = self.service_mgr.lock().unwrap().take() {
//             s.stop();
//         }

//         // This is a total hack to do "drain-like" behavior on DashMap.
//         // `drain` is not natively supported per https://github.com/xacrimon/dashmap/issues/141
//         // In our case, we already stopped the service manager, so we shouldn't
//         // getting more concurrent access to the sessions. Thus, safe to do so.
//         let keys: Vec<_> = self
//             .sessions
//             .iter()
//             .map(|entry| entry.key().clone())
//             .collect();
//         // two stage is needed as remove from DashMap while borrow the key in map will deadlock.
//         let sessions = keys
//             .into_iter()
//             .filter_map(|sid| match self.sessions.remove(&sid) {
//                 Some((_, s)) => Some(s),
//                 None => {
//                     debug!("Session not found: {}", sid);
//                     None
//                 }
//             })
//             .collect::<Vec<_>>();

//         let tasks: Vec<_> = sessions
//             .into_iter()
//             .map(|mut session| {
//                 tokio::spawn(async move {
//                     match session.cleanup().await {
//                         Ok(_) => {
//                             debug!("Session cleaned up successfully.");
//                         }
//                         Err(e) => {
//                             debug!("Failed to cleanup session: {:?}", e);
//                         }
//                     }
//                 })
//             })
//             .collect();

//         join_all(tasks).await;
//     }
// }
