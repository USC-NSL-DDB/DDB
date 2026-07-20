use std::sync::Arc;

use anyhow::{bail, Result};
use flume::Receiver;
use tokio::task::JoinHandle;
use tracing::{debug, error};

use super::{DiscoveryMessageProducer, ServiceInfo};
use crate::{
    dbg_ctrl::ProxyTunnel,
    session::{factory::SessionFactory, supervisor::SessionSupervisor},
};

/// Owns the producer-to-session-admission pipeline for service discovery.
pub(crate) struct DiscoveryRuntime {
    producer: Box<dyn DiscoveryMessageProducer>,
    services: Receiver<ServiceInfo>,
    consumer_task: Option<JoinHandle<()>>,
    proxy_tunnel: Option<ProxyTunnel>,
    factory: SessionFactory,
    supervisor: Arc<SessionSupervisor>,
}

impl DiscoveryRuntime {
    pub(crate) fn new(
        producer: Box<dyn DiscoveryMessageProducer>,
        services: Receiver<ServiceInfo>,
        proxy_tunnel: Option<ProxyTunnel>,
        factory: SessionFactory,
        supervisor: Arc<SessionSupervisor>,
    ) -> Self {
        Self {
            producer,
            services,
            consumer_task: None,
            proxy_tunnel,
            factory,
            supervisor,
        }
    }

    pub(crate) fn start(&mut self) -> Result<()> {
        if self.consumer_task.is_some() {
            bail!("discovery runtime is already started");
        }

        let services = self.services.clone();
        let proxy_tunnel = self.proxy_tunnel.clone();
        let factory = self.factory.clone();
        let supervisor = Arc::downgrade(&self.supervisor);
        self.consumer_task = Some(tokio::spawn(async move {
            while let Ok(info) = services.recv_async().await {
                let Some(supervisor) = supervisor.upgrade() else {
                    break;
                };
                debug!(?info, "received discovered service");
                let process = match factory.create_discovered(info, proxy_tunnel.clone()) {
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
        }));
        Ok(())
    }

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        let producer_result = self.producer.stop_producing().await;

        if let Some(task) = self.consumer_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.services.drain();

        producer_result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use async_trait::async_trait;

    use super::*;
    use crate::{
        cmd_flow::{api::CommandExecutor, event::DebuggerEventReducer, router::Router},
        common::Config,
        group_operation::GroupOperationCoordinator,
        notification::NotificationManager,
        runtime_model::RuntimeModel,
        shutdown::ShutdownCtrl,
        source::{
            catalog::SourceCatalog,
            resolver::{SourceResolutionPolicy, SourceResolver},
        },
    };

    struct TestProducer {
        stopped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl DiscoveryMessageProducer for TestProducer {
        async fn start_producing(&mut self, _tx: flume::Sender<ServiceInfo>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stop_producing(&mut self) -> anyhow::Result<()> {
            self.stopped.store(true, Ordering::Release);
            Ok(())
        }
    }

    #[tokio::test]
    async fn runtime_starts_once_and_stops_its_producer() {
        let config = Arc::new(Config::default());
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        let backend = crate::debugger::resolve_debugger_backend(config.as_ref());
        let model = RuntimeModel::new();
        let notifications = Arc::new(NotificationManager::new());
        let reducer = DebuggerEventReducer::new(Arc::clone(&model), Arc::clone(&notifications));
        let factory =
            SessionFactory::new(Arc::clone(&config), backend, Arc::clone(&plugin), reducer);
        let router = Arc::new(Router::new(Arc::clone(&model)));
        let source_resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::clone(model.groups()),
            CommandExecutor::new(Arc::clone(&router)),
            SourceResolutionPolicy::OnDemand,
        );
        let supervisor = SessionSupervisor::new(
            config,
            plugin,
            model,
            router,
            notifications,
            Arc::new(GroupOperationCoordinator::new()),
            source_resolver,
            Arc::new(ShutdownCtrl::new()),
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let (_services_tx, services) = flume::bounded(1);
        let mut runtime = DiscoveryRuntime::new(
            Box::new(TestProducer {
                stopped: Arc::clone(&stopped),
            }),
            services,
            None,
            factory,
            supervisor,
        );

        runtime.start().expect("runtime should start");
        assert_eq!(
            runtime
                .start()
                .expect_err("runtime should reject a second start")
                .to_string(),
            "discovery runtime is already started"
        );
        runtime.shutdown().await.expect("runtime should stop");

        assert!(stopped.load(Ordering::Acquire));
    }
}
