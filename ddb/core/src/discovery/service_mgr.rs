// use anyhow::Result;
// use flume::{Receiver, Sender};
// use rumqttc::{Event, Packet};
// use std::sync::Arc;
// use std::sync::Mutex;
// use tokio::sync::watch;
// use tracing::{debug, error, info};

// use super::{
//     broker::{BrokerInfo, MessageBroker},
//     subscriber::AsyncDiscoverClient,
//     ServiceInfo,
// };
// use crate::common::sd_defaults;

// const BUFFER_CAPACITY: usize = 1024;

// pub struct ServiceMgr {
//     sig_stop: watch::Sender<bool>,
//     sender: Sender<rumqttc::Event>,
//     receiver: Receiver<rumqttc::Event>,

//     broker: Option<Box<dyn MessageBroker>>,

//     callback: Option<Arc<dyn Fn(NewServiceEventType) + Send + Sync>>,
//     handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
// }

// pub type NewServiceEventType = ServiceInfo;

// impl ServiceMgr {
//     fn new() -> Self {
//         let (sig_stop, _) = watch::channel(false);
//         let (sender, receiver) = flume::bounded(BUFFER_CAPACITY);
//         ServiceMgr {
//             sig_stop,
//             sender,
//             receiver,
//             broker: None,
//             callback: None,
//             handles: Mutex::new(Vec::new()),
//         }
//     }

//     fn set_broker(&mut self, broker: Box<dyn MessageBroker>) {
//         self.broker = Some(broker);
//     }

//     fn set_callback<F>(&mut self, cb: F)
//     where
//         F: Fn(NewServiceEventType) + Send + Sync + 'static,
//     {
//         self.callback = Some(Arc::new(cb));
//     }

//     fn monitor(&self, mut client: AsyncDiscoverClient) -> tokio::task::JoinHandle<()> {
//         let stop_rx = self.sig_stop.subscribe();
//         let sender = self.sender.clone();

//         tokio::spawn(async move {
//             // We should respect ExactlyOnce semantics.
//             if let Ok(_) = client
//                 .subscribe(sd_defaults::T_SERVICE_DISCOVERY, rumqttc::QoS::ExactlyOnce)
//                 .await
//             {
//                 if let Err(e) = client.handle(sender, stop_rx).await {
//                     error!("Client handler error: {}", e);
//                 }
//             } else {
//                 debug!(
//                     "Failed to subscribe to topic: {}",
//                     sd_defaults::T_SERVICE_DISCOVERY
//                 );
//             }
//         })
//     }

//     fn spawn_consumer(&self) {
//         let receiver = self.receiver.clone();
//         let cb = self.callback.clone();

//         let h = tokio::spawn(async move {
//             while let Ok(event) = receiver.recv_async().await {
//                 if let Event::Incoming(Packet::Publish(publish)) = event {
//                     // Avoid allocation if possible by using str_split
//                     if let Ok(payload_str) = std::str::from_utf8(&publish.payload) {
//                         if let Some(ref cb) = cb {
//                             (cb)(payload_str.into());
//                         } else {
//                             debug!("No callback set for event: {:?}, proceed with no-op. Only valid for testing.", publish);
//                         }
//                     }
//                 } else {
//                     // ignore other events
//                 }
//             }
//         });
//         self.handles.lock().unwrap().push(h);
//     }

//     pub fn stop(&self) {
//         self.handles.lock().unwrap().iter().for_each(|h| {
//             h.abort();
//         });

//         if let Err(e) = self.sig_stop.send(true) {
//             error!("Failed to send stop signal: {}", e);
//         }

//         if let Some(broker) = &self.broker {
//             if let Err(e) = broker.stop() {
//                 error!("Failed to stop broker: {}", e);
//             }
//         }
//     }
// }

// // Builder pattern implementation
// #[derive(Default)]
// pub struct ServiceMgrBuilder {
//     broker_host: Option<String>,
//     concurrency: Option<usize>,
//     handler: Option<Box<dyn Fn(NewServiceEventType) + Send + Sync>>,
//     managed_broker: Option<Box<dyn MessageBroker>>,
//     check_broker_online: bool,
// }

// impl ServiceMgrBuilder {
//     pub fn new() -> Self {
//         Self::default()
//     }

//     // pub fn with_handler(mut self, handler: Box<dyn Fn(NewServiceEventType) + Send + Sync>) -> Self
//     // {
//     //     self.handler = Some(Arc::new(handler));
//     //     self
//     // }

//     pub fn with_handler<F>(mut self, handler: F) -> Self
//     where
//         F: Fn(NewServiceEventType) + Send + Sync + 'static,
//     {
//         self.handler = Some(Box::new(handler));
//         self
//     }

//     pub fn broker_host(mut self, host: impl Into<String>) -> Self {
//         self.broker_host = Some(host.into());
//         self
//     }

//     pub fn concurrency(mut self, concurrency: usize) -> Self {
//         self.concurrency = Some(concurrency);
//         self
//     }

//     pub fn managed_broker(mut self, broker: Box<dyn MessageBroker>) -> Self {
//         self.managed_broker = Some(broker);
//         self
//     }

//     pub fn check_broker_online(mut self, check: bool) -> Self {
//         self.check_broker_online = check;
//         self
//     }

//     pub fn build(self) -> Result<ServiceMgr> {
//         let broker_host = self
//             .broker_host
//             .unwrap_or_else(|| sd_defaults::DEFAULT_BROKER_HOSTNAME.to_string());

//         let mut service_mgr = ServiceMgr::new();

//         if let Some(broker) = self.managed_broker {
//             info!("Starting managed broker...");
//             let bi: BrokerInfo = BrokerInfo {
//                 hostname: broker_host.clone(),
//                 port: sd_defaults::BROKER_PORT,
//             };
//             broker.start(&bi)?;
//             service_mgr.set_broker(broker);
//         }

//         let concurrency = self.concurrency.unwrap_or(3);

//         let handler = match self.handler {
//             Some(h) => h,
//             None => {
//                 return Err(anyhow::anyhow!("Handler is not set"));
//             }
//         };

//         let mut client = AsyncDiscoverClient::new(
//             sd_defaults::CLIENT_ID,
//             &broker_host,
//             sd_defaults::BROKER_PORT,
//         );

//         if self.check_broker_online {
//             // Use block_in_place to avoid creating a new runtime
//             let result = tokio::task::block_in_place(|| {
//                 tokio::runtime::Handle::current()
//                     .block_on(async { client.check_broker_online().await })
//             });

//             match result {
//                 Ok(_) => {
//                     info!("Broker is online");
//                 }
//                 Err(e) => {
//                     return Err(e);
//                 }
//             }
//         }

//         // Spawn the monitor task
//         service_mgr.monitor(client);

//         // Set the callback
//         service_mgr.set_callback(handler);

//         // Spawn consumer tasks
//         for _ in 0..concurrency {
//             service_mgr.spawn_consumer();
//         }

//         Ok(service_mgr)
//     }
// }
