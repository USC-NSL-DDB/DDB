use std::path::Path;
use std::{net::Ipv4Addr, sync::Arc};

use super::discovery_message_producer::{DiscoveryMessageProducer, ServiceInfo};
use crate::connection::ssh_client_channel::SSHProxyCred;
use crate::dbg_ctrl::dbg_bridge_ctrl::SSHAttachController;
use anyhow::Result;
use flume::Sender;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{AttachParams, ListParams, WatchEvent, WatchParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Api, Client, Config};
use russh::client::Handle;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// A Producer that uses MQTT (via `AsyncDiscoverClient`) to receive
/// `ServiceInfo` events and send them through a channel.
pub struct K8sProducer {
    /// We'll use this watch channel to gracefully tell our tasks to stop.
    sig_stop: watch::Sender<bool>,

    /// Keep track of spawned tasks for `start_producing`. We'll abort them in `stop_producing`.
    handles: Vec<JoinHandle<()>>,

    config: crate::common::config::Config,

    tunnel: Arc<Handle<crate::connection::ssh_client_channel::SSHProxyClientHandler>>,

    service_name: String,

    // Add client as a field so we don't have to recreate it for each pod
    client: Option<Client>,
}

impl K8sProducer {
    /// Create a new MqttProducer, optionally with an owned broker.
    pub fn new(
        config: crate::common::config::Config,
        tunnel: Arc<Handle<crate::connection::ssh_client_channel::SSHProxyClientHandler>>,
        service_name: String,
    ) -> Self {
        let (sig_stop, _) = watch::channel(false);
        Self {
            sig_stop: sig_stop,
            handles: Vec::new(),
            config,
            tunnel,
            service_name,
            client: None,
        }
    }
}
async fn get_pod_pid(pods: &Api<Pod>, pod_name: &str, service_name: &str) -> Result<i32> {
    // Execute ps command in the pod
    let mut output = String::new();
    let mut attached = pods
        .exec(
            pod_name,
            vec!["ps", "-eo", "pid,comm"],
            &AttachParams::default(),
        )
        .await?;

    if let Some(mut stdout) = attached.stdout() {
        use tokio::io::AsyncReadExt;
        stdout.read_to_string(&mut output).await?;
    }

    // Parse output to find PID
    for line in output.lines() {
        if line.contains(service_name) {
            if let Some(pid_str) = line.split_whitespace().next() {
                if let Ok(pid) = pid_str.parse::<i32>() {
                    return Ok(pid);
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "Could not find PID for service {}",
        service_name
    ))
}
#[axum::async_trait]
impl DiscoveryMessageProducer for K8sProducer {
    /// Start "producing" by:
    /// 1. Optionally starting our broker,
    /// 2. Creating an AsyncDiscoverClient,
    /// 3. Subscribing to the desired topic,
    /// 4. Spawning a monitor task that feeds an internal channel with MQTT events,
    /// 5. Spawning consumer tasks that parse events and send `ServiceInfo` into `tx`.
    async fn start_producing(&mut self, tx: Sender<ServiceInfo>) -> Result<()> {
        let kubeconfig_path =
            Path::new("/home/junzhouh/distributed_debugger/SocialNetwork-serviceweaver/kubeconfig");
        let kubeconfig = Kubeconfig::read_from(kubeconfig_path)?;
        let mut config =
            Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
        config.accept_invalid_certs = true;
        let client = Client::try_from(config)?;
        self.client = Some(client.clone());
        let pods: Api<Pod> = Api::namespaced(client, "default");

        let selector_string = format!("serviceweaver/app={}", self.service_name);
        let label_selector = selector_string.as_str();
        let lp = ListParams::default().labels(label_selector);

        // List existing pods
        let pod_list = pods.list(&lp).await?;
        // println!(
        //     "Found {} pods with label {}:",
        //     pod_list.items.len(),
        //     label_selector
        // );
        // for pod in pod_list.items {
        //     println!(" - {}", pod.metadata.name.unwrap_or_default());
        // }

        // Start watching for pod events
        let wp = WatchParams::default().labels(label_selector);
        let mut stream = pods.watch(&wp, "0").await?.boxed();
        let mut event_count = 0;
        let ssh_user = self.config.ssh.user.clone();

        let service_name = self.service_name.clone();
        let tunnel = self.tunnel.clone();
        tokio::task::spawn(async move {
            while let Ok(Some(status)) = stream.try_next().await {
                match status {
                    WatchEvent::Added(pod) => {
                        let pod_name = pod.metadata.name.unwrap_or_default();
                        // println!("New pod added: {}", pod_name);
                        let pod_status = pod.status.unwrap_or_default();
                        let pod_ip = pod_status.pod_ip.unwrap_or_default();
                        let ip_str = pod_ip.as_str();
                        let ssh_cred = SSHProxyCred::new(
                            ip_str,
                            22,
                            ssh_user.as_str(),
                            None,
                            Some("admin123".to_string()),
                        );

                        // Get PID using Kubernetes exec
                        let tunnel = tunnel.clone();
                        match get_pod_pid(&pods, &pod_name, &service_name).await {
                            Ok(pid) => {
                                let ip: Ipv4Addr =
                                    ip_str.parse().expect("Invalid IP address format");
                                let info: ServiceInfo = ServiceInfo::new(
                                    ip,
                                    ip_str.to_string(),
                                    pid as u64,
                                    "hash".to_string(),
                                    "alias".to_string(),
                                    Box::new(SSHAttachController::new(ssh_cred, tunnel)),
                                );
                                tx.send_async(info).await.ok();
                            }
                            Err(e) => {
                                println!("Failed to get PID for pod {}: {}", pod_name, e);
                            }
                        }
                    }
                    WatchEvent::Modified(_) => {}
                    WatchEvent::Deleted(_) => {}
                    WatchEvent::Bookmark(_) => { /* Ignore bookmarks */ }
                    WatchEvent::Error(e) => {
                        eprintln!("Watch error: {:?}", e);
                        break;
                    }
                }
                event_count += 1;
                // For test purposes, break after a few events
                if event_count >= 10000 {
                    break;
                }
            }
        });

        Ok(())
    }

    /// Stop producing:
    /// 1. Abort all spawned tasks,
    /// 2. Signal "stop" to the monitor task,
    /// 3. Stop the broker if we started it.
    async fn stop_producing(&mut self) -> Result<()> {
        //
        // 1. Abort tasks
        //
        for handle in &self.handles {
            handle.abort();
        }
        self.handles.clear();

        //
        // 2. Send stop signal to the monitor task
        //
        let _ = self.sig_stop.send(true);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use futures::{StreamExt, TryStreamExt};
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{ListParams, WatchEvent, WatchParams};
    use kube::config::{KubeConfigOptions, Kubeconfig};
    use kube::{Api, Client, Config};

    #[tokio::test]
    async fn test_list_and_watch_pods() -> Result<(), Box<dyn std::error::Error>> {
        // Initialize Kubernetes client (assumes a valid KUBECONFIG or in-cluster configuration)
        let kubeconfig_path =
            Path::new("/home/junzhouh/distributed_debugger/SocialNetwork-serviceweaver/kubeconfig");
        let kubeconfig = Kubeconfig::read_from(kubeconfig_path)?;
        let mut config =
            Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default()).await?;
        config.accept_invalid_certs = true;
        let client = Client::try_from(config)?;
        let pods: Api<Pod> = Api::namespaced(client, "default");

        // Define label selector for pods with serviceweaver/name equal to "my-label"
        let label_selector = "serviceweaver/app=server.out";
        let lp = ListParams::default().labels(label_selector);

        // List existing pods
        let pod_list = pods.list(&lp).await?;
        println!(
            "Found {} pods with label {}:",
            pod_list.items.len(),
            label_selector
        );
        for pod in pod_list.items {
            println!(" - {}", pod.metadata.name.unwrap_or_default());
        }
        // Start watching for pod events
        // For testing, we'll process a limited number of events
        // For testing, we'll process a limited number of events
        let wp = WatchParams::default().labels(label_selector);
        let mut stream = pods.watch(&wp, "0").await?.boxed();
        let mut event_count = 0;
        while let Some(status) = stream.try_next().await? {
            match status {
                WatchEvent::Added(pod) => {
                    // Specify the namespace and pod name
                    let namespace = "default";
                    let pod_name = pod.metadata.name.unwrap_or_default();
                    println!("Try to get PID for pod: {}", pod_name);
                    let pid = get_pod_pid(&pods, &pod_name, "server.out").await?;
                    println!("New pod added: {}", pod_name);
                    println!("PID: {}", pid);
                }
                WatchEvent::Modified(pod) => {
                    println!("Pod modified: {}", pod.metadata.name.unwrap_or_default());
                }
                WatchEvent::Deleted(pod) => {
                    println!("Pod deleted: {}", pod.metadata.name.unwrap_or_default());
                }
                WatchEvent::Bookmark(_) => { /* Ignore bookmarks */ }
                WatchEvent::Error(e) => {
                    eprintln!("Watch error: {:?}", e);
                    break;
                }
            }
            event_count += 1;
            // For test purposes, break after a few events
            if event_count >= 10000 {
                break;
            }
        }

        // Test passes if the code runs without panicking.
        Ok(())
    }
}
