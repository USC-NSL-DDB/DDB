use flume::Receiver;
use tracing::debug;

use super::{DiscoveryMessageProducer, ServiceInfo};

/// Owns a discovery producer and the stream of services it reports.
///
/// Discovery only observes: consuming the stream and admitting sessions is
/// the debugger manager's job.
pub(crate) struct DiscoveryRuntime {
    producer: Box<dyn DiscoveryMessageProducer>,
    services: Receiver<ServiceInfo>,
}

impl DiscoveryRuntime {
    pub(crate) fn new(
        producer: Box<dyn DiscoveryMessageProducer>,
        services: Receiver<ServiceInfo>,
    ) -> Self {
        Self { producer, services }
    }

    pub(crate) fn services(&self) -> Receiver<ServiceInfo> {
        self.services.clone()
    }

    pub(crate) async fn shutdown(&mut self) -> anyhow::Result<()> {
        debug!("stopping discovery producer");
        let producer_result = self.producer.stop_producing().await;
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
    async fn shutdown_stops_the_producer_and_drains_pending_services() {
        let stopped = Arc::new(AtomicBool::new(false));
        let (services_tx, services) = flume::bounded(1);
        services_tx
            .send(ServiceInfo::new(
                std::net::Ipv4Addr::LOCALHOST,
                "svc".to_string(),
                42,
                "hash".to_string(),
                "api".to_string(),
                None,
            ))
            .unwrap();
        let mut runtime = DiscoveryRuntime::new(
            Box::new(TestProducer {
                stopped: Arc::clone(&stopped),
            }),
            services,
        );

        runtime.shutdown().await.expect("runtime should stop");

        assert!(stopped.load(Ordering::Acquire));
        assert!(runtime.services().is_empty());
    }
}
