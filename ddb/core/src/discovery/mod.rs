pub mod broker;
pub mod service_mgr;
pub mod subscriber;
pub mod discovery_message_producer;
pub mod mqtt_producer;
pub mod k8s_producer;
pub use discovery_message_producer::{DiscoveryMessageProducer, ServiceInfo};


#[cfg(test)]
mod tests {
    use super::broker::MosquittoBroker;
    use super::broker::{BrokerInfo, MessageBroker};
    use std::process::Command;

    #[test]
    fn test_broker_starter() {
        let config_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/conf/mosquitto.conf");
        assert!(config_path.exists());

        let broker = MosquittoBroker::new(config_path.into_os_string().into_string().unwrap());
        broker
            .start(&BrokerInfo {
                hostname: "localhost".to_string(),
                port: 10101,
            })
            .expect("Failed to start broker");

        let output = Command::new("pgrep")
            .arg("mosquitto")
            .output()
            .expect("Failed to execute command");

        assert!(output.status.success(), "Mosquitto process is not running");

        std::thread::sleep(std::time::Duration::from_secs(2));

        broker.stop().expect("Failed to stop broker");
    }

    // #[test]
    // fn test_ip_conversion() {
    //     let ip_int: u32 = 2130706433;
    //     let tag = "hello";
    //     let pid = 1234 as u64;

    //     let info_str = format!("{}:{}:{}", ip_int, tag, pid);
    //     let info: ServiceInfo = info_str.as_str().into();
    //     assert_eq!(info.ip, Ipv4Addr::new(127, 0, 0, 1));
    //     assert_eq!(info.tag, tag);
    //     assert_eq!(info.pid, pid);
    // }
}
