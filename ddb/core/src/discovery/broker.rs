use anyhow::{Context, Result};
use std::fs::{self};
use std::io::Write;
use std::process::Command;
use tempfile::NamedTempFile;
use thiserror::Error;
use tracing::{debug, error, info};

use crate::common::config::ManagedBrokerConfig;
use crate::Asset;
use crate::common::utils::run_command;

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
}

impl MosquittoBroker {
    pub fn new() -> Self {
        Self {
            temp_config_file: None,
        }
    }
}

impl MessageBroker for MosquittoBroker {
    fn start(&mut self, broker_info: &BrokerInfo) -> Result<()> {
        let config_path = broker_info.broker_config.as_ref().and_then(|brker_conf| {
            brker_conf.config_path.as_ref()
        });
        
        let mqtt_conf_path = match config_path {
            Some(path) => path,
            None => {
                let conf = Asset::get("conf/mosquitto.conf").context("Failed to get Mosquitto config file")?;
                let temp_conf_file = extract_embedded_file_content(conf.data.as_ref())?;
                self.temp_config_file = Some(temp_conf_file);
                self.temp_config_file.as_ref().unwrap().path().to_str().unwrap()
            }
        };
        
        fs::create_dir_all("/tmp/ddb/brokers/mosquitto")?;
        let result = run_command::<true, true>("mosquitto", &["-c", mqtt_conf_path, "-d"]);
        match result {
            Ok(_) => {
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
        // Try with sudo first, fall back to regular pkill if sudo is not available
        // let kill_result = if Command::new("which").arg("sudo").status().is_ok() {
        //     Command::new("sudo")
        //         .args(["pkill", "-9", "mosquitto"])
        //         .stdout(Stdio::null())
        //         .stderr(Stdio::null())
        //         .status()
        // } else {
        //     Command::new("pkill").args(["-9", "mosquitto"]).status()
        // };
        let kill_result = Command::new("pkill").args(["-9", "mosquitto"]).status();
        match kill_result {
            Ok(_) => {
                debug!("Mosquitto broker terminated successfully!");
                Ok(())
            }
            Err(e) => {
                error!("Failed to terminate Mosquitto broker: {}", e);
                Err(BrokerError::StopError(e.to_string()).into())
            }
        }
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
        let config_path = broker_info.broker_config.as_ref().and_then(|brker_conf| {
            brker_conf.config_path.as_ref()
        });
        let mqtt_conf_path = match config_path {
            Some(path) => path,
            None => {
                let conf = Asset::get("conf/emqx.conf").context("Failed to get EMQX config file")?;
                let temp_conf_file = extract_embedded_file_content(conf.data.as_ref())?;
                self.temp_config_file = Some(temp_conf_file);
                self.temp_config_file.as_ref().unwrap().path().to_str().unwrap()
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
