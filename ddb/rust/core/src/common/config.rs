use super::default_vals;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tracing::debug;

// Global configuration instance
static mut GLOBAL_CONFIG: Option<Config> = None;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(rename = "PreTasks", default)]
    pub pre_tasks: Vec<Task>,

    #[serde(rename = "PostTasks", default)]
    pub post_tasks: Vec<Task>,

    #[serde(rename = "Framework", default)]
    pub framework: Framework,

    #[serde(rename = "PrerunGdbCommands", default)]
    pub prerun_gdb_cmds: Vec<GdbCommand>,

    #[serde(rename = "PostrunGdbCommands", default)]
    pub postrun_gdb_cmds: Vec<GdbCommand>,

    #[serde(rename = "SSH", default)]
    pub ssh: SshConfig,

    #[serde(rename = "ServiceDiscovery", default)]
    pub service_discovery: Option<ServiceDiscovery>,

    #[serde(rename = "Conf", default)]
    pub conf: Conf,

    #[serde(rename = "ServiceWeaverConf", default)]
    pub service_weaver_conf: Option<ServiceWeaverConf>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ServiceWeaverConf {
    pub service_name: String,
    pub jump_clinet_host: String,
    pub jump_client_port: u16,
    pub jump_client_user: String,
    pub jump_client_password: String,
    pub jump_client_key_path: String,
}
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Conf {
    #[serde(default)]
    pub sudo: bool,
    #[serde(default)]
    pub on_exit: OnExit,
    #[serde(default = "default_api_svr_port")]
    pub api_server_port: u16,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
    #[serde(default = "default_base_dir")]
    pub base_dir: String,
    #[serde(default)]
    pub support_migration: bool,
}

impl Default for Conf {
    fn default() -> Self {
        Self {
            sudo: false,
            on_exit: OnExit::default(),
            api_server_port: default_vals::DEFAULT_API_SVR_PORT,
            log_dir: default_vals::DEFAULT_LOG_DIR.to_string(),
            base_dir: default_vals::DEFAULT_BASE_DIR.to_string(),
            support_migration: false, // TODO: default to true when testing is done.
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OnExit {
    DETACH,
    KILL,
}

impl Default for OnExit {
    fn default() -> Self {
        Self::DETACH
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    Nu,
    ServiceWeaverKube,
    GRPC,
    #[serde(other)]
    Unspecified,
}

impl Default for Framework {
    fn default() -> Self {
        default_vals::DEFAULT_FRAMEWORK
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Task {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct GdbCommand {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SshConfig {
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ServiceDiscovery {
    #[serde(rename = "Broker", default)]
    pub broker: BrokerConfig,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct BrokerConfig {
    #[serde(default = "default_broker_hostname")]
    pub hostname: String,
    #[serde(default)]
    pub managed: Option<ManagedBrokerConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BrokerType {
    Emqx,
    Mosquitto,
    #[serde(other)]
    Unknown,
}

impl Default for BrokerType {
    fn default() -> Self {
        BrokerType::Emqx
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ManagedBrokerConfig {
    #[serde(rename = "type", default)]
    pub broker_type: BrokerType,
}

fn default_ssh_user() -> String {
    default_vals::DEFAULT_SSH_USER.clone()
}

fn default_ssh_port() -> u16 {
    default_vals::DEFAULT_SSH_PORT
}

fn default_api_svr_port() -> u16 {
    default_vals::DEFAULT_API_SVR_PORT
}

fn default_log_dir() -> String {
    default_vals::DEFAULT_LOG_DIR.to_string()
}

fn default_base_dir() -> String {
    default_vals::DEFAULT_BASE_DIR.to_string()
}

fn default_broker_hostname() -> String {
    use super::sd_defaults;
    sd_defaults::DEFAULT_BROKER_HOSTNAME.to_string()
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            user: default_ssh_user(),
            port: default_ssh_port(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pre_tasks: Vec::new(),
            post_tasks: Vec::new(),
            framework: Framework::default(),
            prerun_gdb_cmds: Vec::new(),
            postrun_gdb_cmds: Vec::new(),
            ssh: SshConfig::default(),
            service_discovery: None,
            conf: Conf::default(),
            service_weaver_conf: None,
        }
    }
}

#[allow(dead_code)]
impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load configuration from a YAML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let contents = fs::read_to_string(path)?;
        let conf = Self::from_str(&contents);
        conf
    }

    /// Parse configuration from a YAML string
    pub fn from_str(contents: &str) -> Result<Self> {
        let config = serde_yml::from_str(contents)?;
        Ok(config)
    }

    /// Save configuration to a YAML file
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let yaml = serde_yml::to_string(self)?;
        fs::write(path, yaml)?;
        Ok(())
    }

    /// Initialize the global configuration with a file path
    ///
    /// # Safety
    ///
    /// This function should only be called once during program initialization
    /// before any other threads are spawned. It is unsafe because it modifies
    /// a global mutable static variable.
    ///
    /// # Arguments
    ///
    /// * `path` - An optional file path from which to load the configuration.
    ///           If `None`, the default configuration is used.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration file cannot be read or parsed.
    ///
    /// # Example
    ///
    /// ```rust
    /// unsafe {
    ///     Config::init_global(Some("/path/to/config.yaml")).expect("Failed to initialize global config");
    /// }
    /// ```
    pub unsafe fn init_global<P: AsRef<Path>>(path: Option<P>) -> Result<()> {
        let config = match path {
            Some(p) => Self::from_file(p)?,
            None => Self::default(),
        };
        debug!("Initializing global config: {:?}", config);
        GLOBAL_CONFIG = Some(config);
        Ok(())
    }

    /// Get a reference to the global configuration
    /// SAFETY: This is safe to call only after init_global has been called
    #[allow(static_mut_refs)]
    pub fn global() -> &'static Config {
        // SAFETY: This is safe because:
        // 1. We know init_global was called first
        // 2. We never modify GLOBAL_CONFIG after initialization
        // 3. All accesses are read-only
        unsafe { GLOBAL_CONFIG.as_ref().unwrap() }
    }
}
