use anyhow::{Context, Result};
use std::fs::{self};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use tempfile::NamedTempFile;
use thiserror::Error;
use tracing::{debug, error, info};

use crate::common::config::ManagedBrokerConfig;
use crate::common::utils::run_command;
use crate::Asset;

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("Failed to write config file: {0}")]
    ConfigWriteError(#[from] std::io::Error),
    #[error("Broker not found: {0}")]
    BrokerNotFound(String),
    #[error("Failed to start broker: {0}")]
    StartError(String),
    #[error("Failed to stop broker: {0}")]
    StopError(String),
}

#[derive(Debug, Clone)]
pub struct BrokerInfo {
    pub hostname: String,
    pub port: u16,
    pub broker_config: Option<ManagedBrokerConfig>,
}

pub trait MessageBroker: Send + Sync {
    fn start(&mut self, broker_info: &BrokerInfo) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}

#[inline]
fn extract_embedded_file_content(content: &[u8]) -> Result<NamedTempFile> {
    // Create temporary file
    let mut temp_file = NamedTempFile::new().context("Failed to create temporary file")?;

    temp_file
        .write_all(content)
        .context("Failed to write script content")?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o755))
            .context("Failed to set script permissions")?;
    }
    Ok(temp_file)
}

// Mosquitto implementation
pub struct MosquittoBroker {
    temp_config_file: Option<NamedTempFile>,
    child: Option<Child>,
}

impl MosquittoBroker {
    pub fn new() -> Self {
        Self {
            temp_config_file: None,
            child: None,
        }
    }
}

impl MessageBroker for MosquittoBroker {
    fn start(&mut self, broker_info: &BrokerInfo) -> Result<()> {
        anyhow::ensure!(self.child.is_none(), "managed Mosquitto is already started");
        // Refuse an occupied listener instead of silently adopting another broker.
        let reservation = std::net::TcpListener::bind(("0.0.0.0", broker_info.port))
            .context("managed Mosquitto port is already occupied")?;
        let config_path = broker_info
            .broker_config
            .as_ref()
            .and_then(|brker_conf| brker_conf.config_path.as_ref());

        let mqtt_conf_path = match config_path {
            Some(path) => path.to_string(),
            None => {
                let conf = format!(
                    "listener {}\nallow_anonymous true\npersistence false\nlog_dest stderr\n",
                    broker_info.port
                );
                let temp_conf_file = extract_embedded_file_content(conf.as_bytes())?;
                self.temp_config_file = Some(temp_conf_file);
                self.temp_config_file
                    .as_ref()
                    .context("temporary broker config was not retained")?
                    .path()
                    .to_str()
                    .context("temporary broker config path is not UTF-8")?
                    .to_string()
            }
        };

        drop(reservation);
        let result = Command::new("mosquitto")
            .args(["-c", &mqtt_conf_path])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn();
        match result {
            Ok(child) => {
                self.child = Some(child);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
                        self.child = None;
                        anyhow::bail!("managed Mosquitto exited before readiness: {status}");
                    }
                    if std::net::TcpStream::connect((
                        broker_info.hostname.as_str(),
                        broker_info.port,
                    ))
                    .is_ok()
                    {
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        self.stop()?;
                        anyhow::bail!("managed Mosquitto did not listen on its configured port");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                info!("Mosquitto broker started successfully!");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                error!("Mosquitto program not found. Please make sure it is installed.");
                Err(BrokerError::BrokerNotFound("mosquitto".to_string()).into())
            }
            Err(e) => {
                error!("Failed to start Mosquitto broker: {}", e);
                Err(BrokerError::StartError(e.to_string()).into())
            }
        }
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            if child.try_wait()?.is_none() {
                child.kill()?;
            }
            child.wait()?;
            debug!("owned Mosquitto broker terminated");
        }
        Ok(())
    }
}

impl Drop for MosquittoBroker {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(all(test, unix))]
mod mosquitto_tests {
    use super::*;
    #[test]
    fn shutdown_only_terminates_the_owned_process_and_is_idempotent() {
        let mut unrelated = Command::new("sleep").arg("30").spawn().unwrap();
        let mut broker = MosquittoBroker::new();
        broker.child = Some(Command::new("sleep").arg("30").spawn().unwrap());
        broker.stop().unwrap();
        broker.stop().unwrap();
        let unrelated_running = unrelated.try_wait().unwrap().is_none();
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        assert!(
            unrelated_running,
            "another process must survive broker shutdown"
        );
    }
}

pub struct EMQXBroker {
    temp_config_file: Option<NamedTempFile>,
}

impl EMQXBroker {
    pub fn new() -> Self {
        Self {
            temp_config_file: None,
        }
    }
}

impl MessageBroker for EMQXBroker {
    fn start(&mut self, broker_info: &BrokerInfo) -> Result<()> {
        let config_path = broker_info
            .broker_config
            .as_ref()
            .and_then(|brker_conf| brker_conf.config_path.as_ref());
        let mqtt_conf_path = match config_path {
            Some(path) => path.to_string(),
            None => {
                let conf =
                    Asset::get("conf/emqx.conf").context("Failed to get EMQX config file")?;
                let temp_conf_file = extract_embedded_file_content(conf.data.as_ref())?;
                self.temp_config_file = Some(temp_conf_file);
                self.temp_config_file
                    .as_ref()
                    .context("temporary broker config was not retained")?
                    .path()
                    .to_str()
                    .context("temporary broker config path is not UTF-8")?
                    .to_string()
            }
        };

        // Stop and remove the existing `emqx` container
        run_command::<true, true>("docker", &["rm", "-f", "emqx"]).ok();

        fs::create_dir_all("/tmp/ddb/brokers/emqx/data")?;
        fs::create_dir_all("/tmp/ddb/brokers/emqx/logs")?;

        let uid = Command::new("id").arg("-u").output()?.stdout;
        let gid = Command::new("id").arg("-g").output()?.stdout;

        let user = format!(
            "{}:{}",
            String::from_utf8(uid)?.trim(),
            String::from_utf8(gid)?.trim()
        );

        let conf_mount = format!("{}:/opt/emqx/etc/emqx.conf", mqtt_conf_path);
        let mut docker_cmds = vec![
            "run",
            "-d",
            "--name",
            "emqx",
            "-p",
            "10101:10101",
            "-p",
            "8083:8083",
            "-p",
            "8084:8084",
            "-p",
            "8883:8883",
            "-p",
            "18083:18083",
            "-v",
            "/tmp/ddb/brokers/emqx/data:/opt/emqx/data",
            "-v",
            "/tmp/ddb/brokers/emqx/logs:/opt/emqx/log",
            "-v",
            &conf_mount,
            "--user",
            &user,
        ];
        if let Some(broker_conf) = &broker_info.broker_config {
            docker_cmds.extend(broker_conf.emqx_flags.iter().map(String::as_str));
            docker_cmds.push(&broker_conf.emqx_image);
        } else {
            docker_cmds.push("emqx/emqx:5.8.4");
        }

        // Start the new EMQX container
        let result = run_command::<true, true>("docker", &docker_cmds);
        match result {
            Ok(_) => {
                info!("EMQX broker started successfully!");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                error!("EMQX program not found. Please make sure it is installed.");
                Err(BrokerError::BrokerNotFound("emqx".to_string()).into())
            }
            Err(e) => {
                error!("Failed to start EMQX broker: {}", e);
                Err(BrokerError::StartError(e.to_string()).into())
            }
        }
    }

    fn stop(&mut self) -> Result<()> {
        let start = std::time::Instant::now();
        match run_command::<true, true>("docker", &["rm", "-f", "emqx"]) {
            Ok(_) => {
                debug!("EMQX broker stopped successfully!");
                debug!("Time taken to stop EMQX broker: {:?}", start.elapsed());
                Ok(())
            }
            Err(e) => {
                error!("Failed to stop EMQX broker: {}", e);
                debug!("Time taken to stop EMQX broker: {:?}", start.elapsed());
                Err(BrokerError::StopError(e.to_string()).into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_embedded_file_content_writes_bytes_to_temp_file() {
        let script = b"#!/bin/sh\necho hello\n";

        let temp_file = extract_embedded_file_content(script).expect("temp file should be created");

        assert_eq!(std::fs::read(temp_file.path()).unwrap(), script);
    }

    #[cfg(unix)]
    #[test]
    fn extract_embedded_file_content_marks_temp_file_executable() {
        use std::os::unix::fs::PermissionsExt;

        let temp_file =
            extract_embedded_file_content(b"#!/bin/sh\necho hello\n").expect("temp file");
        let mode = std::fs::metadata(temp_file.path())
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(mode & 0o111, 0o111);
    }
}
