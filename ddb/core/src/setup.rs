use crate::{
    common::{config::Config, default_vals},
    debugger::{install_bundled_assets, DebuggerBackend},
    logging,
};
use std::sync::Arc;

use crate::logging::TracingGuards;
use anyhow::{Context, Result};
use tracing::{debug, info};

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

#[derive(Debug, Clone)]
pub struct LoggingSettings {
    pub console_log: bool,
    pub console_level: String,
    pub file_level: String,
    pub otel_endpoint: String,
    pub otel_level: String,
    pub enable_otel: bool,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        LoggingSettings {
            console_log: false,
            console_level: "info".to_string(),
            file_level: "info".to_string(),
            otel_endpoint: "http://127.0.0.1:54317".to_string(),
            otel_level: "info".to_string(),
            enable_otel: true,
            user_id: None,
            session_id: None,
        }
    }
}

impl LoggingSettings {
    pub fn from_args(args: &crate::arg::Args) -> Self {
        LoggingSettings {
            console_log: args.console_log,
            console_level: args.console_level.clone(),
            file_level: args.file_level.clone(),
            otel_endpoint: args.otel_endpoint.clone(),
            otel_level: args.otel_level.clone(),
            enable_otel: args.enable_otel,
            user_id: args.user_id.clone(),
            session_id: args.session_id.clone(),
        }
    }
}

pub struct SetupProcedure {
    config: Arc<Config>,
    backend: Arc<dyn DebuggerBackend>,
    app_dir_config: AppDirConfig,
    logging_settings: LoggingSettings,
}

impl SetupProcedure {
    pub fn new(config: Arc<Config>, backend: Arc<dyn DebuggerBackend>) -> Self {
        SetupProcedure {
            config,
            backend,
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

    pub fn run(&mut self) -> Result<TracingGuards> {
        // Create directories
        self.app_dir_config.create_dirs()?;

        // Setup logging with OpenTelemetry tracing
        let guards = logging::setup_logging(
            crate::global::APP_NAME,
            self.app_dir_config.get_log_dir(),
            &self.logging_settings,
        )?;

        let config = self.config.as_ref();
        let backend_assets = self.backend.bundled_assets(config);
        let installed_backend_assets = install_bundled_assets(&backend_assets)?;

        if config.handle_migration() {
            let path = installed_backend_assets
                .last()
                .cloned()
                .unwrap_or_else(|| std::path::PathBuf::from("<not-installed>"));
            info!(
                "feature: [ENABLED] proclet migration. Debugger runtime written to: {}",
                path.display()
            );
        } else {
            info!("feature: [DISABLED] proclet migration.");
        }

        // print out some heads-up
        #[cfg(feature = "lazy_source_map")]
        info!("[FEATURE]: (ENABLED) lazy source map");
        #[cfg(not(feature = "lazy_source_map"))]
        info!("[FEATURE]: (DISABLED) lazy source map");

        Ok(guards)
    }
}
