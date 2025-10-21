use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::NamedTempFile;
use thiserror::Error;
use tracing::{debug, error, info};

use crate::common::sd_defaults;
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

fn write_config(broker: &BrokerInfo) -> Result<()> {
    let path = Path::new(sd_defaults::SERVICE_DISCOVERY_INI_FILEPATH);
    let mut file = File::create(path)?;

    writeln!(
        file,
        "{}://{}:{}\n{}\n",
        sd_defaults::BROKER_MSG_TRANSPORT,
        broker.hostname,
        broker.port,
        sd_defaults::T_SERVICE_DISCOVERY
    )?;

    Ok(())
}

#[derive(Debug, Clone)]
pub struct BrokerInfo {
    pub hostname: String,
    pub port: u16,
}

pub trait MessageBroker: Send + Sync {
    fn start(&self, broker_info: &BrokerInfo) -> Result<()>;
    fn stop(&self) -> Result<()>;
}

// Mosquitto implementation
pub struct MosquittoBroker {
    config_path: String,
}

impl MosquittoBroker {
    pub fn new(config_path: String) -> Self {
        Self { config_path }
    }
}

impl MessageBroker for MosquittoBroker {
    fn start(&self, broker_info: &BrokerInfo) -> Result<()> {
        // Write configuration
        write_config(broker_info)?;

        // Start broker
        match Command::new("mosquitto")
            .arg("-c")
            .arg(&self.config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
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

    fn stop(&self) -> Result<()> {
        // Try with sudo first, fall back to regular pkill if sudo is not available
        let kill_result = if Command::new("which").arg("sudo").status().is_ok() {
            Command::new("sudo")
                .args(["pkill", "-9", "mosquitto"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        } else {
            Command::new("pkill").args(["-9", "mosquitto"]).status()
        };

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

pub struct EMQXBroker;

impl EMQXBroker {
    pub fn new() -> Self {
        Self {}
    }
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

impl MessageBroker for EMQXBroker {
    fn start(&self, broker_info: &BrokerInfo) -> Result<()> {
        use crate::common::utils::run_command;

        // Write configuration
        write_config(broker_info)?;

        let conf = Asset::get("conf/emqx.conf").context("Failed to get EMQX config file")?;
        let temp_conf_file = extract_embedded_file_content(conf.data.as_ref())?;

        // Stop and remove the existing `emqx` container
        run_command::<true>("docker", &["stop", "emqx"]).ok();
        run_command::<true>("docker", &["rm", "emqx"]).ok();

        fs::create_dir_all("/tmp/ddb/emqx/data")?;
        fs::create_dir_all("/tmp/ddb/emqx/logs")?;

        let uid = Command::new("id").arg("-u").output()?.stdout;
        let gid = Command::new("id").arg("-g").output()?.stdout;

        let user = format!(
            "{}:{}",
            String::from_utf8(uid)?.trim(),
            String::from_utf8(gid)?.trim()
        );

        let conf_path = temp_conf_file.path().to_str().unwrap();

        // Start the new EMQX container
        run_command::<true>(
            "docker",
            &[
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
                "/tmp/ddb/emqx/data:/opt/emqx/data",
                "-v",
                "/tmp/ddb/emqx/logs:/opt/emqx/log",
                "-v",
                &format!("{}:/opt/emqx/etc/emqx.conf", conf_path),
                "--user",
                &user,
                "emqx/emqx:5.8.4",
            ],
        )?;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        match Command::new("/bin/bash")
            .arg("-c")
            .arg("docker stop emqx && docker rm emqx")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => {
                debug!("EMQX broker stop successfully!");
                Ok(())
            }
            Err(e) => {
                error!("Failed to stop EMQX broker: {}", e);
                Err(BrokerError::StopError(e.to_string()).into())
            }
        }
    }
}
