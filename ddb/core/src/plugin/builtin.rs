use std::sync::Arc;

use ddb_api_extension::{ExtensionProvider, ExtensionSchema, ProviderError};
use ddb_api_types::v2::{
    extension_payload, ExtensionColumnDescriptor, ExtensionDescriptor, ExtensionPayload,
    ExtensionPresentationDescriptor, ExtensionPresentationKind, PermissionScope,
};
use sha2::{Digest, Sha256};

use crate::common::config::{Config, Framework};
use crate::state::RuntimeModel;

use super::{
    DebuggerBootstrapAction, FrameworkDebuggerBootstrap, FrameworkPlugin, GrpcAdapter, NuAdapter,
    ServiceDiscoveryMode, ServiceWeaverAdapter,
};

const PROCLET_MIGRATION_EXTENSION: &str = "ddb.proclet_migration";
const PROCLET_OWNERS_PANEL: &str = "proclet_owners";
const PROCLET_MIGRATION_SCHEMA_URI: &str = "urn:ddb:extension:ddb.proclet_migration:v1";
const PROCLET_MIGRATION_SCHEMA: &[u8] =
    include_bytes!("../../schemas/extensions/proclet-migration-v1.schema.json");

#[derive(Debug)]
struct ProcletMigrationExtension {
    model: Arc<RuntimeModel>,
}

impl ExtensionProvider for ProcletMigrationExtension {
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            extension_id: PROCLET_MIGRATION_EXTENSION.to_string(),
            owner: "DDB".to_string(),
            version: "1".to_string(),
            title: "Proclet migration".to_string(),
            description: "Framework proclet ownership and heap-migration state".to_string(),
            schema_uri: PROCLET_MIGRATION_SCHEMA_URI.to_string(),
            schema_hash: Sha256::digest(PROCLET_MIGRATION_SCHEMA)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            required_scopes: vec![PermissionScope::Read as i32],
            actions: Vec::new(),
            events: Vec::new(),
            presentations: vec![ExtensionPresentationDescriptor {
                id: PROCLET_OWNERS_PANEL.to_string(),
                title: "Proclet ownership".to_string(),
                description: None,
                kind: ExtensionPresentationKind::Table as i32,
                columns: vec![
                    ExtensionColumnDescriptor {
                        id: "proclet".to_string(),
                        title: "Proclet".to_string(),
                        value_type: Some("string".to_string()),
                    },
                    ExtensionColumnDescriptor {
                        id: "session".to_string(),
                        title: "Session".to_string(),
                        value_type: Some("string".to_string()),
                    },
                ],
                action_id: None,
            }],
            minimum_api_version: Some("v2".to_string()),
            maximum_api_version: None,
        }
    }

    fn schemas(&self) -> Vec<ExtensionSchema> {
        vec![ExtensionSchema {
            uri: PROCLET_MIGRATION_SCHEMA_URI.to_string(),
            media_type: "application/schema+json".to_string(),
            content: PROCLET_MIGRATION_SCHEMA.to_vec(),
        }]
    }

    fn state(&self) -> Result<Vec<ExtensionPayload>, ProviderError> {
        let rows = self
            .model
            .proclet_owners()
            .into_iter()
            .map(|(proclet_id, session_id)| vec![proclet_id.to_string(), session_id.to_string()])
            .collect::<Vec<_>>();
        let payload_json = serde_json::json!({
            "id": PROCLET_MIGRATION_EXTENSION,
            "panels": [{"id": PROCLET_OWNERS_PANEL, "rows": rows}]
        })
        .to_string();
        Ok(vec![ExtensionPayload {
            extension_id: PROCLET_MIGRATION_EXTENSION.to_string(),
            schema_version: "1".to_string(),
            schema_uri: PROCLET_MIGRATION_SCHEMA_URI.to_string(),
            media_type: "application/json".to_string(),
            payload: Some(extension_payload::Payload::PayloadJson(payload_json)),
        }])
    }
}

fn proclet_migration_extensions(
    config: &Config,
    model: Arc<RuntimeModel>,
) -> Vec<Arc<dyn ExtensionProvider>> {
    if !config.handle_migration() {
        return Vec::new();
    }
    vec![Arc::new(ProcletMigrationExtension { model })]
}

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

    fn debugger_bootstrap(&self, config: &Config) -> FrameworkDebuggerBootstrap {
        let mut bootstrap = FrameworkDebuggerBootstrap {
            requires_proclet_runtime: config.handle_migration(),
            ..FrameworkDebuggerBootstrap::default()
        };
        if config.service_discovery.is_some() {
            bootstrap
                .post_start_actions
                .push(DebuggerBootstrapAction::Signal("SIG40".to_string()));
        }
        if let Some(plugin) = config.plugin.as_ref() {
            bootstrap
                .scripts
                .extend(plugin.debugger_scripts.iter().map(std::path::PathBuf::from));
        }
        bootstrap
    }

    fn api_extensions(
        &self,
        config: &Config,
        model: Arc<RuntimeModel>,
    ) -> Vec<Arc<dyn ExtensionProvider>> {
        proclet_migration_extensions(config, model)
    }
}

impl FrameworkPlugin for QuicksandFrameworkPlugin {
    fn command_adapter(&self) -> Arc<dyn super::FrameworkCommandAdapter> {
        Arc::new(NuAdapter)
    }

    fn supports_migration(&self, config: &Config) -> bool {
        config.conf.support_migration
    }

    fn debugger_bootstrap(&self, config: &Config) -> FrameworkDebuggerBootstrap {
        let mut bootstrap = FrameworkDebuggerBootstrap {
            requires_proclet_runtime: config.handle_migration(),
            ..FrameworkDebuggerBootstrap::default()
        };
        if let Some(plugin) = config.plugin.as_ref() {
            bootstrap
                .scripts
                .extend(plugin.debugger_scripts.iter().map(std::path::PathBuf::from));
        }
        bootstrap
    }

    fn api_extensions(
        &self,
        config: &Config,
        model: Arc<RuntimeModel>,
    ) -> Vec<Arc<dyn ExtensionProvider>> {
        proclet_migration_extensions(config, model)
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
        let mut bootstrap = FrameworkDebuggerBootstrap::default();
        if config.service_discovery.is_some() {
            bootstrap
                .post_start_actions
                .push(DebuggerBootstrapAction::Signal("SIG40".to_string()));
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
        let mut config = Config {
            framework: Framework::Quicksand,
            ..Config::default()
        };
        config.conf.support_migration = true;

        let plugin = resolve_framework_plugin(&config);
        assert!(plugin.supports_migration(&config));

        let bootstrap = plugin.debugger_bootstrap(&config);
        assert!(bootstrap.requires_proclet_runtime);
        assert!(bootstrap.scripts.is_empty());
        assert_eq!(plugin.api_extensions(&config, RuntimeModel::new()).len(), 1);
    }

    #[test]
    fn default_framework_has_no_ui_extensions() {
        let config = Config::default();
        let plugin = resolve_framework_plugin(&config);

        assert!(plugin
            .api_extensions(&config, RuntimeModel::new())
            .is_empty());
    }

    #[test]
    fn grpc_plugin_bootstrap_keeps_sig40_post_start_action() {
        let mut config = Config::default();
        config.framework = Framework::GRPC;
        config.service_discovery = Some(crate::common::config::ServiceDiscovery::default());

        let plugin = resolve_framework_plugin(&config);
        let bootstrap = plugin.debugger_bootstrap(&config);

        assert_eq!(
            bootstrap.post_start_actions,
            vec![DebuggerBootstrapAction::Signal("SIG40".to_string())]
        );
    }
}
