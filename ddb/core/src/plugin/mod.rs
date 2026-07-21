mod builtin;

use std::{
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use gdbmi::raw::Value;

use crate::{
    common::config::{Config, DebuggerCommand},
    debugger::gdb::runtime::CORE_GDB_RUNTIME_ASSET,
    debugger::gdb::runtime::PROCLET_GDB_RUNTIME_ASSET,
    debugger::BundledDebuggerAsset,
};

pub use builtin::resolve_framework_plugin;

pub trait FrameworkCommandAdapter: Send + Sync + std::fmt::Debug {
    fn get_bt_command_name(&self) -> String;
    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String>;
}

#[derive(Clone, Debug)]
pub struct GrpcAdapter;

impl FrameworkCommandAdapter for GrpcAdapter {
    fn get_bt_command_name(&self) -> String {
        "-get-remote-bt".to_string()
    }

    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String> {
        let pid = meta.get_dict_entry("pid")?.expect_string_repr::<u64>()?;
        let ip_int = meta.get_dict_entry("ip")?.expect_string_repr::<u32>()?;
        let ip_str = Ipv4Addr::from(ip_int).to_string();
        Ok(format!("{}:-{}", ip_str, pid))
    }
}

#[derive(Clone, Debug)]
pub struct NuAdapter;

impl FrameworkCommandAdapter for NuAdapter {
    fn get_bt_command_name(&self) -> String {
        "-get-remote-bt".to_string()
    }

    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String> {
        let pid = meta.get_dict_entry("pid")?.expect_string_repr::<u64>()?;
        let ip_int = meta.get_dict_entry("ip")?.expect_string_repr::<u32>()?;
        let ip_str = Ipv4Addr::from(ip_int).to_string();
        Ok(format!("{}:-{}", ip_str, pid))
    }
}

#[derive(Clone, Debug)]
pub struct ServiceWeaverAdapter;

impl FrameworkCommandAdapter for ServiceWeaverAdapter {
    fn get_bt_command_name(&self) -> String {
        "-serviceweaver-bt-remote".to_string()
    }

    fn extract_id_from_metadata(&self, meta: &Value) -> Result<String> {
        let ip_int = meta.get_dict_entry("ip")?.expect_string_repr::<u32>()?;
        let ip_str = Ipv4Addr::from(ip_int).to_string();
        Ok(ip_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceDiscoveryMode {
    None,
    MessageBroker,
    Kubernetes,
}

#[derive(Debug, Clone, Default)]
pub struct FrameworkDebuggerBootstrap {
    pub scripts: Vec<PathBuf>,
    pub pre_attach_commands: Vec<DebuggerCommand>,
    pub post_start_commands: Vec<DebuggerCommand>,
}

pub trait FrameworkPlugin: Send + Sync + std::fmt::Debug {
    fn command_adapter(&self) -> Arc<dyn FrameworkCommandAdapter>;
    fn service_discovery_mode(&self, _config: &Config) -> ServiceDiscoveryMode {
        ServiceDiscoveryMode::None
    }
    fn supports_migration(&self, _config: &Config) -> bool {
        false
    }
    fn should_register_caladan_ip(&self, config: &Config) -> bool {
        self.supports_migration(config)
    }
    fn bundled_assets(&self, _config: &Config) -> Vec<BundledDebuggerAsset> {
        Vec::new()
    }
    fn debugger_bootstrap(&self, config: &Config) -> FrameworkDebuggerBootstrap {
        let mut bootstrap = FrameworkDebuggerBootstrap::default();
        if let Some(plugin) = config.plugin.as_ref() {
            bootstrap
                .scripts
                .extend(plugin.debugger_scripts.iter().map(PathBuf::from));
        }
        bootstrap
    }
}

pub(crate) fn runtime_script_path(asset: &BundledDebuggerAsset) -> PathBuf {
    Path::new(asset.output_dir).join(asset.file_name)
}

pub(crate) fn default_runtime_asset() -> BundledDebuggerAsset {
    CORE_GDB_RUNTIME_ASSET
}

pub(crate) fn proclet_runtime_asset() -> BundledDebuggerAsset {
    PROCLET_GDB_RUNTIME_ASSET
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use gdbmi::raw::{Dict, Value};

    #[derive(Debug)]
    struct DummyPlugin;

    impl FrameworkPlugin for DummyPlugin {
        fn command_adapter(&self) -> Arc<dyn FrameworkCommandAdapter> {
            Arc::new(GrpcAdapter)
        }
    }

    fn remote_meta(ip: u32, pid: u64) -> Value {
        Value::from(Dict::from(HashMap::from([
            ("ip", Value::from(ip.to_string())),
            ("pid", Value::from(pid.to_string())),
        ])))
    }

    #[test]
    fn adapters_extract_expected_remote_identifiers() {
        let remote = remote_meta(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 42);

        assert_eq!(
            GrpcAdapter.extract_id_from_metadata(&remote).unwrap(),
            "127.0.0.1:-42"
        );
        assert_eq!(
            NuAdapter.extract_id_from_metadata(&remote).unwrap(),
            "127.0.0.1:-42"
        );
        assert_eq!(
            ServiceWeaverAdapter
                .extract_id_from_metadata(&remote)
                .unwrap(),
            "127.0.0.1"
        );
    }

    #[test]
    fn default_framework_bootstrap_includes_configured_plugin_scripts() {
        let mut config = Config::default();
        config.plugin = Some(crate::common::config::PluginConfig {
            debugger_scripts: vec!["/tmp/a.py".to_string(), "/tmp/b.py".to_string()],
        });

        let bootstrap = DummyPlugin.debugger_bootstrap(&config);

        assert_eq!(
            bootstrap.scripts,
            vec![PathBuf::from("/tmp/a.py"), PathBuf::from("/tmp/b.py")]
        );
        assert!(bootstrap.pre_attach_commands.is_empty());
        assert!(bootstrap.post_start_commands.is_empty());
    }
}
