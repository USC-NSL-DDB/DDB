//! Explicit, ordered session setup. Local source browsing is configured separately.

use std::{io::Read, path::Path};

use anyhow::{ensure, Context, Result};
use ddb_api_client::v2;
use serde::Deserialize;

use crate::{
    api::{ApiClient, CapabilitiesExt},
    cli::ValuesFormatting,
    model::UiMessage,
};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Install a typed temporary function breakpoint before native commands.
    entry_function: Option<String>,
    #[serde(default)]
    source_mappings: Vec<Mapping>,
    #[serde(default)]
    gdb: Backend,
    #[serde(default)]
    lldb: Backend,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    from: String,
    to: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backend {
    #[serde(default)]
    commands: Vec<String>,
}

#[derive(Debug)]
pub struct Setup {
    settings: Vec<v2::configure_debugger_request::Setting>,
    entry_function: Option<String>,
    commands: Vec<String>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let mut text = String::new();
        std::fs::File::open(path)
            .with_context(|| format!("open debugger settings {}", path.display()))?
            .take(65_537)
            .read_to_string(&mut text)?;
        Self::parse(&text).with_context(|| format!("debugger settings {}", path.display()))
    }

    fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= 65_536, "settings exceed 64 KiB");
        let settings: Self = serde_yml::from_str(text)?;
        if let Some(function) = &settings.entry_function {
            ensure!(
                !function.trim().is_empty() && function.len() <= 4096 && !function.chars().any(char::is_control),
                "entry_function must be nonempty, at most 4096 bytes, and contain no control characters"
            );
        }
        ensure!(
            settings.source_mappings.len() <= 128,
            "too many source mappings"
        );
        for mapping in &settings.source_mappings {
            for path in [&mapping.from, &mapping.to] {
                ensure!(
                    !path.trim().is_empty() && path.len() <= 4_096 && !path.chars().any(char::is_control),
                    "source mapping paths must be nonempty, at most 4096 bytes, and contain no control characters"
                );
            }
        }
        for backend in [&settings.gdb, &settings.lldb] {
            ensure!(backend.commands.len() <= 128, "too many startup commands");
            for command in &backend.commands {
                ensure!(
                    !command.trim().is_empty() && !command.chars().any(char::is_control),
                    "startup commands must be nonempty single-line debugger commands"
                );
            }
        }
        Ok(settings)
    }

    pub fn plan(&self, kind: i32, formatting: ValuesFormatting) -> Option<Setup> {
        let backend = match v2::BackendKind::try_from(kind).ok()? {
            v2::BackendKind::Gdb => &self.gdb,
            v2::BackendKind::Lldb => &self.lldb,
            _ => return None,
        };
        let mut settings = Vec::new();
        if kind == v2::BackendKind::Gdb as i32 && formatting == ValuesFormatting::PrettyPrinters {
            settings
                .push(v2::configure_debugger_request::Setting::EnablePrettyPrinting(v2::Empty {}));
        }
        settings.extend(self.source_mappings.iter().map(|mapping| {
            v2::configure_debugger_request::Setting::SourceMapping(v2::SourcePathMapping {
                from: mapping.from.clone(),
                to: mapping.to.clone(),
            })
        }));
        (!settings.is_empty() || !backend.commands.is_empty() || self.entry_function.is_some())
            .then(|| Setup {
                settings,
                entry_function: self.entry_function.clone(),
                commands: backend.commands.clone(),
            })
    }
}

impl Setup {
    pub fn supported(&self, capabilities: &v2::Capabilities) -> bool {
        (self.entry_function.is_none()
            || (capabilities.supports_breakpoint_action("create_function")
                && capabilities.supports_breakpoint_action("temporary")))
            && (self.settings.is_empty()
                || capabilities
                    .supported_operations
                    .contains(&(v2::OperationKind::ConfigureDebugger as i32)))
            && (self.commands.is_empty()
                || capabilities
                    .supported_operations
                    .contains(&(v2::OperationKind::RawCommand as i32)))
    }

    pub async fn run(
        self,
        client: &ApiClient,
        messages: &tokio::sync::mpsc::Sender<UiMessage>,
        session: &str,
    ) -> Result<()> {
        let target = Some(crate::api::session_target(session));
        for setting in self.settings {
            let admission = client
                .configure_debugger(v2::ConfigureDebuggerRequest {
                    target: target.clone(),
                    setting: Some(setting),
                    ..Default::default()
                })
                .await?;
            report(client, messages, session, admission).await?;
        }
        if let Some(function_name) = self.entry_function {
            let admission = client
                .create_breakpoint(v2::CreateBreakpointRequest {
                    target: target.clone(),
                    breakpoint: Some(v2::BreakpointSpec {
                        location: Some(v2::breakpoint_spec::Location::Function(
                            v2::FunctionBreakpointLocation { function_name },
                        )),
                        enabled: Some(true),
                        temporary: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .await?;
            report(client, messages, session, admission).await?;
        }
        for command in self.commands {
            let admission = client
                .execute_raw_command(v2::ExecuteRawCommandRequest {
                    target: target.clone(),
                    dialect: v2::RawCommandDialect::BackendNative as i32,
                    command,
                    ..Default::default()
                })
                .await?;
            report(client, messages, session, admission).await?;
        }
        Ok(())
    }
}

async fn report(
    client: &ApiClient,
    messages: &tokio::sync::mpsc::Sender<UiMessage>,
    session: &str,
    admission: v2::OperationAdmissionResponse,
) -> Result<()> {
    let receipt = crate::worker::complete_operation(client, admission).await?;
    let success = receipt.state == v2::OperationState::Completed as i32;
    messages
        .send(UiMessage::Receipt(
            format!("debugger setup for {session}"),
            receipt,
        ))
        .await?;
    ensure!(success, "Debugger setup for {session} stopped after a failed operation; remaining settings/commands were not applied");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_only_setup_is_planned_for_both_native_backends() {
        let settings =
            Settings::parse("entry_function: 'Widget::operator new(unsigned long)'\n").unwrap();
        for backend in [v2::BackendKind::Gdb, v2::BackendKind::Lldb] {
            let plan = settings
                .plan(backend as i32, ValuesFormatting::Raw)
                .unwrap();
            assert_eq!(
                plan.entry_function.as_deref(),
                Some("Widget::operator new(unsigned long)")
            );
            assert!(plan.settings.is_empty());
            assert!(plan.commands.is_empty());
        }
    }

    #[test]
    fn plans_preserve_mapping_order_and_select_only_the_matching_backend_commands() {
        let settings = Settings::parse("source_mappings:\n  - {from: '/build with spaces', to: '/src'}\n  - {from: '/other', to: '/more'}\ngdb:\n  commands: ['set print elements 33']\nlldb:\n  commands: ['settings set target.max-children-count 33']\n").unwrap();
        let gdb = settings
            .plan(
                v2::BackendKind::Gdb as i32,
                ValuesFormatting::PrettyPrinters,
            )
            .unwrap();
        assert_eq!(gdb.settings.len(), 3);
        assert!(
            matches!(&gdb.settings[1], v2::configure_debugger_request::Setting::SourceMapping(mapping) if mapping.from == "/build with spaces")
        );
        assert_eq!(gdb.commands, ["set print elements 33"]);
        let lldb = settings
            .plan(
                v2::BackendKind::Lldb as i32,
                ValuesFormatting::PrettyPrinters,
            )
            .unwrap();
        assert_eq!(lldb.settings.len(), 2);
        assert_eq!(lldb.commands, ["settings set target.max-children-count 33"]);
        assert!(settings
            .plan(v2::BackendKind::Mock as i32, ValuesFormatting::Raw)
            .is_none());
    }

    #[test]
    fn settings_reject_typos_control_characters_empty_commands_and_oversize_input() {
        for text in [
            "autorun: []",
            "gdb: {command: []}",
            "gdb: {commands: [' ']}\n",
            "source_mappings: [{from: '', to: '/src'}]",
            "source_mappings: [{from: ' ', to: '/src'}]",
            "gdb: {commands: [\"continue\\nquit\"]}",
            "entry_function: ''",
            "entry_function: ' '",
            "entry_function: \"main\\ncontinue\"",
        ] {
            assert!(Settings::parse(text).is_err(), "{text}");
        }
        assert!(Settings::parse(&" ".repeat(65_537)).is_err());
        assert!(Settings::parse(&format!("entry_function: {}", "a".repeat(4097))).is_err());
        assert!(Settings::default()
            .plan(v2::BackendKind::Gdb as i32, ValuesFormatting::Raw)
            .is_none());
    }

    #[tokio::test]
    async fn setup_uses_typed_session_requests_in_order_and_stops_after_failure() {
        use axum::{routing::post, Json, Router};
        use std::sync::{Arc, Mutex};
        for failure in [None, Some("mapping"), Some("entry")] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let settings_calls = calls.clone();
            let command_calls = calls.clone();
            let entry_calls = calls.clone();
            let router = Router::new()
                .route(
                    "/api/v2/rpc/ddb.api.v2.DebuggerControlService/ConfigureDebugger",
                    post(move |Json(request): Json<v2::ConfigureDebuggerRequest>| {
                        let calls = settings_calls.clone();
                        async move {
                            assert_eq!(
                                request.target,
                                Some(crate::api::session_target("session/owner"))
                            );
                            let Some(v2::configure_debugger_request::Setting::SourceMapping(
                                mapping,
                            )) = request.setting
                            else {
                                panic!("expected typed source mapping")
                            };
                            assert_eq!(mapping.from, "/build with spaces");
                            assert_eq!(mapping.to, "/source with spaces");
                            calls.lock().unwrap().push("mapping".to_string());
                            Json(admission(if failure == Some("mapping") {
                                v2::OperationState::Failed
                            } else {
                                v2::OperationState::Completed
                            }))
                        }
                    }),
                )
                .route(
                    "/api/v2/rpc/ddb.api.v2.DebuggerControlService/CreateBreakpoint",
                    post(move |Json(request): Json<v2::CreateBreakpointRequest>| {
                        let calls = entry_calls.clone();
                        async move {
                            assert_eq!(request.target, Some(crate::api::session_target("session/owner")));
                            let spec = request.breakpoint.unwrap();
                            assert!(spec.temporary);
                            assert_eq!(spec.enabled, Some(true));
                            assert!(matches!(spec.location, Some(v2::breakpoint_spec::Location::Function(function)) if function.function_name == "app::entry"));
                            calls.lock().unwrap().push("entry".into());
                            Json(admission(if failure == Some("entry") { v2::OperationState::Failed } else { v2::OperationState::Completed }))
                        }
                    }),
                )
                .route(
                    "/api/v2/rpc/ddb.api.v2.DebuggerControlService/ExecuteRawCommand",
                    post(move |Json(request): Json<v2::ExecuteRawCommandRequest>| {
                        let calls = command_calls.clone();
                        async move {
                            assert_eq!(
                                request.target,
                                Some(crate::api::session_target("session/owner"))
                            );
                            assert_eq!(
                                request.dialect,
                                v2::RawCommandDialect::BackendNative as i32
                            );
                            assert!(request.frame_id.is_none());
                            calls.lock().unwrap().push(request.command);
                            Json(admission(v2::OperationState::Completed))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let settings = Settings::parse("entry_function: app::entry\nsource_mappings: [{from: '/build with spaces', to: '/source with spaces'}]\ngdb: {commands: ['first', 'second']}\n").unwrap();
            let setup = settings
                .plan(v2::BackendKind::Gdb as i32, ValuesFormatting::Raw)
                .unwrap();
            let mut capabilities = v2::Capabilities::default();
            assert!(!setup.supported(&capabilities));
            capabilities
                .supported_operations
                .push(v2::OperationKind::ConfigureDebugger as i32);
            assert!(!setup.supported(&capabilities));
            capabilities
                .supported_operations
                .push(v2::OperationKind::RawCommand as i32);
            assert!(!setup.supported(&capabilities));
            capabilities
                .supported_operations
                .push(v2::OperationKind::CreateBreakpoint as i32);
            capabilities
                .breakpoint_features
                .push(v2::BreakpointFeature::Function as i32);
            assert!(!setup.supported(&capabilities));
            capabilities
                .breakpoint_features
                .push(v2::BreakpointFeature::Temporary as i32);
            assert!(setup.supported(&capabilities));
            let client = ApiClient::new(crate::api::ClientConfig::new(endpoint)).unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let result = setup.run(&client, &tx, "session/owner").await;
            assert_eq!(result.is_err(), failure.is_some());
            let expected = match failure {
                Some("mapping") => vec!["mapping"],
                Some("entry") => vec!["mapping", "entry"],
                _ => vec!["mapping", "entry", "first", "second"],
            };
            assert_eq!(*calls.lock().unwrap(), expected);
            for _ in expected {
                assert!(
                    matches!(rx.try_recv().unwrap(), UiMessage::Receipt(label, _) if label == "debugger setup for session/owner")
                );
            }
            assert!(rx.try_recv().is_err());
            server.abort();
            let _ = server.await;
        }
    }

    fn admission(state: v2::OperationState) -> v2::OperationAdmissionResponse {
        v2::OperationAdmissionResponse {
            operation: Some(v2::Operation {
                operation_id: "operation/setup".into(),
                state: state as i32,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}
