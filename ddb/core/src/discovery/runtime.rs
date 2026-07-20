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
    factory: SessionFactory<'static>,
    supervisor: Arc<SessionSupervisor>,
}

impl DiscoveryRuntime {
    pub(crate) fn new(
        producer: Box<dyn DiscoveryMessageProducer>,
        services: Receiver<ServiceInfo>,
        proxy_tunnel: Option<ProxyTunnel>,
        factory: SessionFactory<'static>,
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
        let factory = self.factory;
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
        cmd_flow::{api::CommandExecutor, router::Router},
        common::Config,
        group_operation::GroupOperationCoordinator,
        source::{
            catalog::SourceCatalog,
            resolver::{SourceResolutionPolicy, SourceResolver},
        },
        state::GroupMgr,
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
        let config = Box::leak(Box::new(Config::default()));
        let factory = SessionFactory::new(config);
        let router = Arc::new(Router::new());
        let source_resolver = SourceResolver::new(
            Arc::new(SourceCatalog::new()),
            Arc::new(GroupMgr::new()),
            CommandExecutor::new(router),
            SourceResolutionPolicy::OnDemand,
        );
        let supervisor = SessionSupervisor::new(
            config,
            Arc::new(GroupOperationCoordinator::new()),
            source_resolver,
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
