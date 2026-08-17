pub mod gdb;
pub mod lldb;
pub mod mock;
pub mod protocol;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};

use crate::{
    common::config::{Config, DebuggerBackendKind, OnExit},
    plugin::{DebuggerBootstrapAction, FrameworkDebuggerBootstrap, FrameworkPlugin},
    session::SessionRequest,
    Asset,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledDebuggerAsset {
    pub embedded_path: &'static str,
    pub output_dir: &'static str,
    pub file_name: &'static str,
}

impl BundledDebuggerAsset {
    pub fn output_path(&self) -> PathBuf {
        Path::new(self.output_dir).join(self.file_name)
    }
}

pub fn install_bundled_asset(asset: &BundledDebuggerAsset) -> Result<PathBuf> {
    let script_content =
        Asset::get(asset.embedded_path).context("Failed to load embedded debugger asset")?;
    let file_path = asset.output_path();
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).context("Failed to create debugger asset directory")?;
    }
    fs::write(&file_path, script_content.data.as_ref())
        .context("Failed to write debugger asset to disk")?;
    file_path
        .canonicalize()
        .context("Failed to canonicalize debugger asset path")
}

pub fn install_bundled_assets(assets: &[BundledDebuggerAsset]) -> Result<Vec<PathBuf>> {
    assets
        .iter()
        .map(install_bundled_asset)
        .collect::<Result<Vec<_>>>()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebuggerCapabilities {
    pub proclet_migration: bool,
    pub serviceweaver_remote_backtrace: bool,
}

/// Per-session values shared by a backend's bootstrap and protocol codec.
///
/// The channel id is intentionally unpredictable: LLDB records share stdout
/// with the inferior, so a session-specific prefix prevents ordinary inferior
/// output from being accepted as debugger protocol traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebuggerSessionContext {
    channel_id: String,
}

impl DebuggerSessionContext {
    pub fn new() -> Self {
        Self {
            channel_id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    #[cfg(test)]
    pub(crate) fn for_test(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
        }
    }
}

impl Default for DebuggerSessionContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Ordered debugger startup split at the native-to-structured protocol
/// boundary. Only `protocol_prelude` is written without correlation; every
/// semantic command is submitted through the session runtime and validated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebuggerBootstrapPlan {
    pub protocol_prelude: String,
    pub commands: Vec<String>,
}

impl DebuggerBootstrapPlan {
    pub fn commands(commands: Vec<String>) -> Self {
        Self {
            protocol_prelude: String::new(),
            commands,
        }
    }
}

pub trait DebuggerBackend: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> DebuggerCapabilities {
        DebuggerCapabilities::default()
    }
    fn validate_config(&self, config: &Config) -> Result<()> {
        if config.handle_migration() && !self.capabilities().proclet_migration {
            anyhow::bail!(
                "debugger backend '{}' does not support proclet migration",
                self.name()
            );
        }
        if matches!(
            config.framework,
            crate::common::config::Framework::ServiceWeaverKube
        ) && !self.capabilities().serviceweaver_remote_backtrace
        {
            anyhow::bail!(
                "debugger backend '{}' does not support Service Weaver remote backtraces",
                self.name()
            );
        }
        Ok(())
    }
    fn create_protocol(
        &self,
        context: &DebuggerSessionContext,
    ) -> Box<dyn protocol::DebuggerProtocol>;
    fn bundled_assets(&self, config: &Config) -> Vec<BundledDebuggerAsset>;
    fn build_start_command(&self, sudo: bool) -> String;
    fn build_remote_attach_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
        context: &DebuggerSessionContext,
    ) -> Result<DebuggerBootstrapPlan>;
    fn build_local_binary_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
        context: &DebuggerSessionContext,
    ) -> Result<DebuggerBootstrapPlan>;
    fn interrupt_command(&self) -> String;
    fn console_exec_command(&self, command: &str) -> String;
    fn bootstrap_action_command(&self, action: &DebuggerBootstrapAction) -> String;
    fn shutdown_commands(&self, on_exit: &OnExit) -> String;
}

pub fn resolve_debugger_backend(config: &Config) -> anyhow::Result<Arc<dyn DebuggerBackend>> {
    let backend: Arc<dyn DebuggerBackend> = match config.conf.debugger.backend {
        DebuggerBackendKind::Gdb => Arc::new(gdb::GdbBackend),
        DebuggerBackendKind::Lldb => Arc::new(lldb::LldbBackend),
        DebuggerBackendKind::Mock => Arc::new(mock::MockBackend),
        DebuggerBackendKind::Unknown => {
            anyhow::bail!(
                "unsupported debugger backend configured; expected 'gdb', 'lldb', or 'mock'"
            )
        }
    };
    backend.validate_config(config)?;
    Ok(backend)
}

/// Verifies that a configured native debugger is executable before a headless
/// service announces readiness. Legacy interactive startup keeps its historical
/// lazy behavior.
pub async fn preflight_debugger_backend(backend: &dyn DebuggerBackend) -> Result<()> {
    let executable = backend.name();
    if executable == "mock" {
        return Ok(());
    }

    let mut child = tokio::process::Command::new(executable)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "debugger executable {executable:?} is unavailable; install it or select another backend"
            )
        })?;

    let status = match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
        Ok(result) => result.context("failed to wait for debugger preflight")?,
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            anyhow::bail!("debugger executable {executable:?} did not respond within 5 seconds");
        }
    };
    if !status.success() {
        anyhow::bail!(
            "debugger executable {executable:?} failed its availability check with {status}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::common::default_vals::{
        DEFAULT_EMBEDED_GDB_EXT_FRAME_FILTER_PATH, DEFAULT_EMBEDED_GDB_EXT_PATH,
        DEFAULT_GDB_EXT_FRAME_FILTER_NAME, DEFAULT_GDB_EXT_NAME,
    };

    #[test]
    fn bundled_asset_output_path_joins_output_dir_and_file_name() {
        let asset = BundledDebuggerAsset {
            embedded_path: DEFAULT_EMBEDED_GDB_EXT_PATH,
            output_dir: "/tmp/ddb-tests",
            file_name: DEFAULT_GDB_EXT_NAME,
        };

        assert_eq!(
            asset.output_path(),
            Path::new("/tmp/ddb-tests").join(DEFAULT_GDB_EXT_NAME)
        );
    }

    #[test]
    fn install_bundled_asset_writes_expected_embedded_contents() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let output_dir: &'static str = Box::leak(
            dir.path()
                .to_str()
                .expect("path should be valid utf-8")
                .to_string()
                .into_boxed_str(),
        );
        let asset = BundledDebuggerAsset {
            embedded_path: DEFAULT_EMBEDED_GDB_EXT_PATH,
            output_dir,
            file_name: DEFAULT_GDB_EXT_NAME,
        };

        let installed = install_bundled_asset(&asset).expect("asset should install");
        let expected = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join(DEFAULT_EMBEDED_GDB_EXT_PATH),
        )
        .expect("embedded asset should be readable from repository");

        assert_eq!(installed, asset.output_path().canonicalize().unwrap());
        assert_eq!(std::fs::read_to_string(installed).unwrap(), expected);
    }

    #[test]
    fn install_bundled_assets_installs_each_requested_asset() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let output_dir: &'static str = Box::leak(
            dir.path()
                .to_str()
                .expect("path should be valid utf-8")
                .to_string()
                .into_boxed_str(),
        );
        let assets = [
            BundledDebuggerAsset {
                embedded_path: DEFAULT_EMBEDED_GDB_EXT_PATH,
                output_dir,
                file_name: DEFAULT_GDB_EXT_NAME,
            },
            BundledDebuggerAsset {
                embedded_path: DEFAULT_EMBEDED_GDB_EXT_FRAME_FILTER_PATH,
                output_dir,
                file_name: DEFAULT_GDB_EXT_FRAME_FILTER_NAME,
            },
        ];

        let installed = install_bundled_assets(&assets).expect("assets should install");

        assert_eq!(installed.len(), 2);
        assert!(installed.iter().all(|path| path.exists()));
    }

    #[test]
    fn unsupported_backend_capabilities_fail_during_resolution() {
        let mut config = Config::default();
        config.framework = crate::common::config::Framework::Nu;
        config.conf.support_migration = true;
        config.conf.debugger.backend = DebuggerBackendKind::Lldb;

        let error = resolve_debugger_backend(&config)
            .expect_err("LLDB proclet migration should fail before runtime startup");

        assert_eq!(
            error.to_string(),
            "debugger backend 'lldb' does not support proclet migration"
        );

        config.conf.debugger.backend = DebuggerBackendKind::Gdb;
        assert!(resolve_debugger_backend(&config).is_ok());

        config.framework = crate::common::config::Framework::ServiceWeaverKube;
        config.conf.support_migration = false;
        config.conf.debugger.backend = DebuggerBackendKind::Lldb;
        let error = resolve_debugger_backend(&config)
            .expect_err("LLDB Service Weaver support should fail before runtime startup");
        assert_eq!(
            error.to_string(),
            "debugger backend 'lldb' does not support Service Weaver remote backtraces"
        );
    }
}
