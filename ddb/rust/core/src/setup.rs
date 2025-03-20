use crate::{
    common::{default_vals, utils},
    logging,
    common::config::Config,
};

use anyhow::{Context, Result};
use tracing::{debug, info};
use tracing_appender::non_blocking::WorkerGuard;

#[derive(Debug)]
pub struct AppDirConfig {
    base_dir: String,
    log_dir: String,
    service_discover_conf_dir: String,
    gdb_ext_dir: String,
}

impl Default for AppDirConfig {
    fn default() -> Self {
        AppDirConfig {
            base_dir: default_vals::DEFAULT_BASE_DIR.to_string(),
            log_dir: default_vals::DEFAULT_LOG_DIR.to_string(),
            service_discover_conf_dir: default_vals::DEFAULT_SERVICE_DISCOVER_CONF_DIR.to_string(),
            gdb_ext_dir: default_vals::DEFAULT_GDB_EXT_DIR.to_string(),
        }
    }
}

#[allow(unused)]
impl AppDirConfig {
    pub fn builder() -> AppDirConfigBuilder {
        AppDirConfigBuilder::new()
    }

    pub fn get_base_dir(&self) -> &str {
        &self.base_dir
    }

    pub fn get_log_dir(&self) -> &str {
        &self.log_dir
    }

    pub fn get_service_discover_conf_dir(&self) -> &str {
        &self.service_discover_conf_dir
    }

    pub fn get_gdb_ext_dir(&self) -> &str {
        &self.gdb_ext_dir
    }
    pub fn from_config(config: &Config) -> Self {
        AppDirConfig {
            base_dir: config.conf.base_dir.clone(),
            log_dir: config.conf.log_dir.clone(),
            service_discover_conf_dir: default_vals::DEFAULT_SERVICE_DISCOVER_CONF_DIR.to_string(),
            gdb_ext_dir: default_vals::DEFAULT_GDB_EXT_DIR.to_string(),
        }
    }
}

impl AppDirConfig {
    pub fn create_dirs(&self) -> Result<()> {
        debug!("Creating dirs with config: {:?}", self);

        std::fs::create_dir_all(&self.base_dir).context("Failed to create base directory")?;
        std::fs::create_dir_all(&self.log_dir).context("Failed to create log directory")?;
        std::fs::create_dir_all(&self.service_discover_conf_dir)
            .context("Failed to create service discovery conf directory")?;
        std::fs::create_dir_all(&self.gdb_ext_dir).context("Failed to create gdb ext directory")?;
        Ok(())
    }
}

#[allow(unused)]
pub struct AppDirConfigBuilder {
    base_dir: Option<String>,
    log_dir: Option<String>,
    service_discover_conf_dir: Option<String>,
    gdb_ext_dir: Option<String>,
}

#[allow(unused)]
impl AppDirConfigBuilder {
    pub fn new() -> Self {
        AppDirConfigBuilder {
            base_dir: None,
            log_dir: None,
            service_discover_conf_dir: None,
            gdb_ext_dir: None,
        }
    }

    pub fn base_dir(mut self, dir: &str) -> Self {
        self.base_dir = Some(dir.to_string());
        self
    }

    pub fn log_dir(mut self, dir: &str) -> Self {
        self.log_dir = Some(dir.to_string());
        self
    }

    pub fn service_discover_conf_dir(mut self, dir: &str) -> Self {
        self.service_discover_conf_dir = Some(dir.to_string());
        self
    }

    pub fn gdb_ext_dir(mut self, dir: &str) -> Self {
        self.gdb_ext_dir = Some(dir.to_string());
        self
    }

    pub fn build(&self) -> AppDirConfig {
        AppDirConfig {
            base_dir: self
                .base_dir
                .clone()
                .unwrap_or(default_vals::DEFAULT_BASE_DIR.to_string()),
            log_dir: self
                .log_dir
                .clone()
                .unwrap_or(default_vals::DEFAULT_LOG_DIR.to_string()),
            service_discover_conf_dir: self
                .service_discover_conf_dir
                .clone()
                .unwrap_or(default_vals::DEFAULT_SERVICE_DISCOVER_CONF_DIR.to_string()),
            gdb_ext_dir: self
                .gdb_ext_dir
                .clone()
                .unwrap_or(default_vals::DEFAULT_GDB_EXT_DIR.to_string()),
        }
    }
}

#[derive(Default)]
pub struct LoggingSettings {
    pub console_log: bool,
    pub console_level: String,
    pub file_level: String,
}

impl LoggingSettings {
    pub fn from_args(args: &crate::arg::Args) -> Self {
        LoggingSettings {
            console_log: args.console_log,
            console_level: args.console_level.clone(),
            file_level: args.file_level.clone(),
        }
    }
}

pub struct SetupProcedure {
    app_dir_config: AppDirConfig,
    logging_settings: LoggingSettings,
}

impl SetupProcedure {
    pub fn new() -> Self {
        SetupProcedure {
            app_dir_config: AppDirConfig::default(),
            logging_settings: LoggingSettings::default(),
        }
    }

    #[allow(dead_code)]
    pub fn with_app_dir_config(mut self, app_dir_config: AppDirConfig) -> Self {
        self.app_dir_config = app_dir_config;
        self
    }

    pub fn with_logging_settings(mut self, logging_settings: LoggingSettings) -> Self {
        self.logging_settings = logging_settings;
        self
    }

    pub fn run(&mut self) -> Result<WorkerGuard> {
        // Create directories
        self.app_dir_config.create_dirs()?;

        // Write gdb ext script
        // NOTE: this function returns the path to the script file
        // However, the return value is currently not used
        // For now, we assume the script is written to the default location
        utils::gdb::setup_gdb_ext_script()?;

        // Setup logging
        let guard = logging::setup_logging(
            crate::global::APP_NAME,
            self.app_dir_config.get_log_dir(),
            self.logging_settings.console_log,
            &self.logging_settings.console_level,
            &self.logging_settings.file_level,
        )?;

        // print out some heads-up
        #[cfg(feature = "lazy_source_map")]
        info!("feature: [ENABLED] lazy source map");
        #[cfg(not(feature = "lazy_source_map"))]
        info!("feature: [DISABLED] lazy source map");


        Ok(guard)
    }
}

impl Default for SetupProcedure {
    fn default() -> Self {
        SetupProcedure::new()
    }
}
