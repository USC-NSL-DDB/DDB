use std::sync::Arc;

use crate::{
    common::config::{Config, Framework},
    debugger::BundledDebuggerAsset,
};

use super::{
    default_runtime_asset, proclet_runtime_asset, runtime_script_path, FrameworkDebuggerBootstrap,
    FrameworkPlugin, GrpcAdapter, NuAdapter, ServiceDiscoveryMode, ServiceWeaverAdapter,
};

#[derive(Debug, Default)]
struct NuFrameworkPlugin;

#[derive(Debug, Default)]
struct QuicksandFrameworkPlugin;

#[derive(Debug, Default)]
struct GrpcFrameworkPlugin;

#[derive(Debug, Default)]
struct ServiceWeaverFrameworkPlugin;

#[derive(Debug, Default)]
struct UnspecifiedFrameworkPlugin;

impl FrameworkPlugin for NuFrameworkPlugin {
    fn command_adapter(&self) -> Arc<dyn super::FrameworkCommandAdapter> {
        Arc::new(NuAdapter)
    }

    fn service_discovery_mode(&self, _config: &Config) -> ServiceDiscoveryMode {
        ServiceDiscoveryMode::MessageBroker
    }

    fn supports_migration(&self, config: &Config) -> bool {
        config.conf.support_migration
    }

    fn bundled_assets(&self, config: &Config) -> Vec<BundledDebuggerAsset> {
        let mut assets = Vec::new();
        if config.conf.support_migration {
            assets.push(proclet_runtime_asset());
        }
        assets
    }

    fn debugger_bootstrap(&self, config: &Config) -> FrameworkDebuggerBootstrap {
        let mut bootstrap = FrameworkDebuggerBootstrap {
            scripts: vec![runtime_script_path(&default_runtime_asset())],
            ..FrameworkDebuggerBootstrap::default()
        };
        if config.conf.support_migration {
            bootstrap
                .scripts
                .push(runtime_script_path(&proclet_runtime_asset()));
        }
        if config.service_discovery.is_some() {
            bootstrap
                .post_start_commands
                .push(crate::common::config::DebuggerCommand {
                    name: "sig40".to_string(),
                    command: r#"-interpreter-exec console "signal SIG40""#.to_string(),
                });
        }
        if let Some(plugin) = config.plugin.as_ref() {
            bootstrap
                .scripts
                .extend(plugin.debugger_scripts.iter().map(std::path::PathBuf::from));
        }
        bootstrap
    }
}

impl FrameworkPlugin for QuicksandFrameworkPlugin {
    fn command_adapter(&self) -> Arc<dyn super::FrameworkCommandAdapter> {
        Arc::new(NuAdapter)
    }

    fn supports_migration(&self, config: &Config) -> bool {
        config.conf.support_migration
    }

    fn bundled_assets(&self, config: &Config) -> Vec<BundledDebuggerAsset> {
        let mut assets = Vec::new();
        if config.conf.support_migration {
            assets.push(proclet_runtime_asset());
        }
        assets
    }

    fn debugger_bootstrap(&self, config: &Config) -> FrameworkDebuggerBootstrap {
        let mut bootstrap = FrameworkDebuggerBootstrap {
            scripts: vec![runtime_script_path(&default_runtime_asset())],
            ..FrameworkDebuggerBootstrap::default()
        };
        if config.conf.support_migration {
            bootstrap
                .scripts
                .push(runtime_script_path(&proclet_runtime_asset()));
        }
        if let Some(plugin) = config.plugin.as_ref() {
            bootstrap
                .scripts
                .extend(plugin.debugger_scripts.iter().map(std::path::PathBuf::from));
        }
        bootstrap
    }
}

impl FrameworkPlugin for GrpcFrameworkPlugin {
    fn command_adapter(&self) -> Arc<dyn super::FrameworkCommandAdapter> {
        Arc::new(GrpcAdapter)
    }

    fn service_discovery_mode(&self, _config: &Config) -> ServiceDiscoveryMode {
        ServiceDiscoveryMode::MessageBroker
    }

    fn debugger_bootstrap(&self, config: &Config) -> FrameworkDebuggerBootstrap {
        let mut bootstrap = FrameworkDebuggerBootstrap {
            scripts: vec![runtime_script_path(&default_runtime_asset())],
            ..FrameworkDebuggerBootstrap::default()
        };
        if config.service_discovery.is_some() {
            bootstrap
                .post_start_commands
                .push(crate::common::config::DebuggerCommand {
                    name: "sig40".to_string(),
                    command: r#"-interpreter-exec console "signal SIG40""#.to_string(),
                });
        }
        if let Some(plugin) = config.plugin.as_ref() {
            bootstrap
                .scripts
                .extend(plugin.debugger_scripts.iter().map(std::path::PathBuf::from));
        }
        bootstrap
    }
}

impl FrameworkPlugin for ServiceWeaverFrameworkPlugin {
    fn command_adapter(&self) -> Arc<dyn super::FrameworkCommandAdapter> {
        Arc::new(ServiceWeaverAdapter)
    }

    fn service_discovery_mode(&self, _config: &Config) -> ServiceDiscoveryMode {
        ServiceDiscoveryMode::Kubernetes
    }
}

impl FrameworkPlugin for UnspecifiedFrameworkPlugin {
    fn command_adapter(&self) -> Arc<dyn super::FrameworkCommandAdapter> {
        Arc::new(GrpcAdapter)
    }
}

pub fn resolve_framework_plugin(config: &Config) -> Arc<dyn FrameworkPlugin> {
    match config.framework {
        Framework::Nu => Arc::new(NuFrameworkPlugin),
        Framework::Quicksand => Arc::new(QuicksandFrameworkPlugin),
        Framework::GRPC => Arc::new(GrpcFrameworkPlugin),
        Framework::ServiceWeaverKube => Arc::new(ServiceWeaverFrameworkPlugin),
        Framework::Unspecified => Arc::new(UnspecifiedFrameworkPlugin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quicksand_plugin_preserves_migration_behavior() {
        let mut config = Config::default();
        config.framework = Framework::Quicksand;
        config.conf.support_migration = true;

        let plugin = resolve_framework_plugin(&config);
        assert!(plugin.supports_migration(&config));

        let bootstrap = plugin.debugger_bootstrap(&config);
        assert_eq!(bootstrap.scripts.len(), 2);
    }

    #[test]
    fn grpc_plugin_bootstrap_keeps_runtime_script_and_sig40() {
        let mut config = Config::default();
        config.framework = Framework::GRPC;
        config.service_discovery = Some(crate::common::config::ServiceDiscovery::default());

        let plugin = resolve_framework_plugin(&config);
        let bootstrap = plugin.debugger_bootstrap(&config);

        assert_eq!(
            bootstrap.scripts,
            vec![runtime_script_path(&default_runtime_asset())]
        );
        assert_eq!(bootstrap.post_start_commands.len(), 1);
    }
}
