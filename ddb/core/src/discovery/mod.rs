pub mod broker;
pub mod discovery_message_producer;
pub mod k8s_producer;
pub mod mqtt_producer;
pub(crate) mod runtime;
pub mod subscriber;
pub use discovery_message_producer::{DiscoveryMessageProducer, ServiceInfo};

#[cfg(test)]
mod tests {
    use super::broker::MosquittoBroker;
    use super::broker::{BrokerInfo, MessageBroker};
    use std::net::{TcpListener, TcpStream};

    #[test]
    #[ignore] // this is not runnable unless the broker is installed.
    fn test_broker_starter() {
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let mut broker = MosquittoBroker::new();
        broker
            .start(&BrokerInfo {
                hostname: "127.0.0.1".to_string(),
                port,
                broker_config: None,
            })
            .expect("Failed to start broker");

        TcpStream::connect(("127.0.0.1", port)).expect("managed broker must accept connections");

        broker.stop().expect("Failed to stop broker");
        broker
            .stop()
            .expect("Repeated broker shutdown must be safe");
        TcpListener::bind(("0.0.0.0", port)).expect("managed broker must release its listener");
    }
}
