use anyhow::{bail, Result};
use ddb_api_client::v2::{self, target};

pub use ddb_api_client::{
    ClientConfig, DdbClient as ApiClient, OutputSyncItem, OutputSyncOptions,
    ProjectedStateSyncItem, StateSyncOptions,
};

pub const TUI_API_COMPATIBILITY: &str = "API versions [v2], schema >=2.0.0 and <3.0.0";

/// UI intent for the installation scope of a logical breakpoint.
///
/// IDs remain opaque strings; only the SDK's generated public target contract
/// crosses the frontend/backend boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BreakpointTarget {
    Session(String),
    Group(String),
    Broadcast,
    Multiple(Vec<Self>),
}

impl BreakpointTarget {
    pub fn into_api_target(self) -> v2::Target {
        match self {
            Self::Session(session_id) => session_target(session_id),
            Self::Group(group_id) => group_target(group_id),
            Self::Broadcast => v2::Target {
                selector: Some(target::Selector::Broadcast(v2::BroadcastTarget {})),
            },
            Self::Multiple(targets) => v2::Target {
                selector: Some(target::Selector::Multiple(v2::MultipleTarget {
                    targets: targets.into_iter().map(Self::into_api_target).collect(),
                })),
            },
        }
    }
}

pub fn session_target(session_id: impl Into<String>) -> v2::Target {
    v2::Target {
        selector: Some(target::Selector::Session(v2::SessionTarget {
            session_id: session_id.into(),
        })),
    }
}

pub fn thread_target(thread_id: impl Into<String>) -> v2::Target {
    v2::Target {
        selector: Some(target::Selector::Thread(v2::ThreadTarget {
            thread_id: thread_id.into(),
        })),
    }
}

pub fn group_target(group_id: impl Into<String>) -> v2::Target {
    v2::Target {
        selector: Some(target::Selector::Group(v2::GroupTarget {
            group_id: group_id.into(),
        })),
    }
}

pub trait CapabilitiesExt {
    fn validate_for_tui(&self) -> Result<()>;
    fn supports_inspection(&self, capability: &str) -> bool;
    fn supports_execution(&self, action: v2::ExecutionAction) -> bool;
    fn supports_execution_target(&self, action: v2::ExecutionAction, target: &v2::Target) -> bool;
    fn supports_breakpoint_action(&self, action: &str) -> bool;
    fn supports_ddb_feature(&self, capability: &str) -> bool;
}

impl CapabilitiesExt for v2::Capabilities {
    fn validate_for_tui(&self) -> Result<()> {
        if self.server_instance_id.is_empty() {
            bail!("DDB returned capability metadata without a server instance identity");
        }
        let schema_major = self
            .schema_version
            .split('.')
            .next()
            .and_then(|value| value.parse::<u64>().ok());
        if self.api_version != "v2" || schema_major != Some(2) {
            bail!(
                "ddb-tui {} supports {}; DDB advertised API {:?} with schema {:?}. Install a compatible paired release or select a compatible frontend",
                env!("CARGO_PKG_VERSION"),
                TUI_API_COMPATIBILITY,
                self.api_version,
                self.schema_version
            );
        }
        Ok(())
    }

    fn supports_inspection(&self, capability: &str) -> bool {
        match capability {
            "evaluate" => self
                .supported_operations
                .contains(&(v2::OperationKind::Evaluate as i32)),
            // Read capabilities are represented by stable v2 methods and
            // backend errors; they are not mutation capability flags.
            "memory" | "source" | "stack" | "variables" => true,
            _ => false,
        }
    }

    fn supports_execution(&self, action: v2::ExecutionAction) -> bool {
        self.execution_actions.contains(&(action as i32))
    }

    fn supports_execution_target(&self, action: v2::ExecutionAction, target: &v2::Target) -> bool {
        if !self.execution_actions.contains(&(action as i32)) {
            return false;
        }
        let Some(capability) = self
            .execution_action_capabilities
            .iter()
            .find(|capability| capability.action == action as i32)
        else {
            return false;
        };
        let Some(scope) = execution_scope(target) else {
            return false;
        };
        capability.scopes.contains(&(scope as i32))
    }

    fn supports_breakpoint_action(&self, action: &str) -> bool {
        let (feature, operation) = match action {
            "create" => (
                v2::BreakpointFeature::Source,
                Some(v2::OperationKind::CreateBreakpoint),
            ),
            "create_function" => (
                v2::BreakpointFeature::Function,
                Some(v2::OperationKind::CreateBreakpoint),
            ),
            "delete" => (
                v2::BreakpointFeature::Source,
                Some(v2::OperationKind::DeleteBreakpoint),
            ),
            "source" => (v2::BreakpointFeature::Source, None),
            "enable" | "disable" => (
                v2::BreakpointFeature::EnableDisable,
                Some(v2::OperationKind::UpdateBreakpoint),
            ),
            "conditional" => (v2::BreakpointFeature::Condition, None),
            "ignore_count" => (v2::BreakpointFeature::IgnoreCount, None),
            "temporary" => (v2::BreakpointFeature::Temporary, None),
            "hardware" => (v2::BreakpointFeature::Hardware, None),
            "distributed" => (v2::BreakpointFeature::Distributed, None),
            "group_inheritance" => (v2::BreakpointFeature::GroupInheritance, None),
            _ => return false,
        };
        self.breakpoint_features.contains(&(feature as i32))
            && operation
                .is_none_or(|operation| self.supported_operations.contains(&(operation as i32)))
    }

    fn supports_ddb_feature(&self, capability: &str) -> bool {
        self.ddb_features
            .iter()
            .any(|feature| feature == capability)
            || (capability == "distributed_backtrace"
                && self
                    .supported_operations
                    .contains(&(v2::OperationKind::DistributedBacktrace as i32)))
    }
}

/// Developer-facing labels never participate in capability checks or routing.
pub fn execution_action_label(action: v2::ExecutionAction) -> &'static str {
    match action {
        v2::ExecutionAction::Unspecified => "unspecified",
        v2::ExecutionAction::Continue => "continue",
        v2::ExecutionAction::Interrupt => "interrupt",
        v2::ExecutionAction::Next => "next",
        v2::ExecutionAction::StepIn => "step_in",
        v2::ExecutionAction::StepOut => "step_out",
        v2::ExecutionAction::Jump => "jump",
        v2::ExecutionAction::Signal => "send_signal",
        v2::ExecutionAction::ReverseContinue => "reverse_continue",
        v2::ExecutionAction::ReverseNext => "reverse_next",
        v2::ExecutionAction::ReverseStepIn => "reverse_step_in",
    }
}

fn execution_scope(target: &v2::Target) -> Option<v2::ExecutionScopeKind> {
    Some(match target.selector.as_ref()? {
        target::Selector::Thread(_) | target::Selector::CurrentThread(_) => {
            v2::ExecutionScopeKind::Thread
        }
        target::Selector::Session(_)
        | target::Selector::CurrentSession(_)
        | target::Selector::First(_) => v2::ExecutionScopeKind::Session,
        target::Selector::Group(_) => v2::ExecutionScopeKind::Group,
        target::Selector::SessionSet(_)
        | target::Selector::Broadcast(_)
        | target::Selector::Multiple(_) => v2::ExecutionScopeKind::Fanout,
        target::Selector::Operation(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_actions_have_one_typed_mapping_and_display_labels_are_separate() {
        use crate::model::Control;
        for (control, action, label) in [
            (Control::Continue, v2::ExecutionAction::Continue, "continue"),
            (
                Control::Interrupt,
                v2::ExecutionAction::Interrupt,
                "interrupt",
            ),
            (Control::Next, v2::ExecutionAction::Next, "next"),
            (Control::StepIn, v2::ExecutionAction::StepIn, "step_in"),
            (Control::StepOut, v2::ExecutionAction::StepOut, "step_out"),
        ] {
            assert_eq!(control.execution_action(), Some(action));
            assert_eq!(control.action_name(), Some(label));
        }
        for control in [
            Control::CycleScope,
            Control::Refresh,
            Control::RefreshStack,
            Control::RevealFrame,
        ] {
            assert!(control.execution_action().is_none());
            assert!(control.action_name().is_none());
        }
    }

    #[test]
    fn breakpoint_target_keeps_opaque_identifiers() {
        let target = BreakpointTarget::Group("group/alpha:7".to_string());
        assert_eq!(target.into_api_target(), group_target("group/alpha:7"));
        assert_ne!(
            group_target("group/alpha:7"),
            session_target("group/alpha:7")
        );
    }

    #[test]
    fn capability_checks_use_typed_enum_values() {
        let capabilities = v2::Capabilities {
            api_version: "v2".to_string(),
            schema_version: "2.0.0-draft.3".to_string(),
            server_instance_id: "server".to_string(),
            execution_actions: vec![v2::ExecutionAction::Next as i32],
            execution_action_capabilities: vec![v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Next as i32,
                scopes: vec![v2::ExecutionScopeKind::Thread as i32],
            }],
            breakpoint_features: vec![
                v2::BreakpointFeature::Source as i32,
                v2::BreakpointFeature::EnableDisable as i32,
                v2::BreakpointFeature::Distributed as i32,
                v2::BreakpointFeature::GroupInheritance as i32,
            ],
            supported_operations: vec![
                v2::OperationKind::Evaluate as i32,
                v2::OperationKind::CreateBreakpoint as i32,
                v2::OperationKind::UpdateBreakpoint as i32,
            ],
            ..Default::default()
        };
        capabilities.validate_for_tui().unwrap();
        assert!(capabilities.supports_execution(v2::ExecutionAction::Next));
        assert!(capabilities
            .supports_execution_target(v2::ExecutionAction::Next, &thread_target("thread/a")));
        assert!(!capabilities
            .supports_execution_target(v2::ExecutionAction::Next, &group_target("group/a")));
        assert!(!capabilities
            .supports_execution_target(v2::ExecutionAction::Next, &session_target("session/a")));
        assert!(capabilities.supports_breakpoint_action("disable"));
        assert!(capabilities.supports_breakpoint_action("create"));
        assert!(!capabilities.supports_breakpoint_action("delete"));
        assert!(capabilities.supports_breakpoint_action("distributed"));
        assert!(capabilities.supports_breakpoint_action("group_inheritance"));
        assert!(!capabilities.supports_breakpoint_action("hardware"));
        assert!(capabilities.supports_inspection("evaluate"));
        assert!(!capabilities.supports_execution(v2::ExecutionAction::Continue));
    }

    #[test]
    fn execution_requires_an_advertised_action_scope() {
        let capabilities = v2::Capabilities {
            execution_actions: vec![v2::ExecutionAction::Next as i32],
            ..Default::default()
        };
        assert!(!capabilities
            .supports_execution_target(v2::ExecutionAction::Next, &thread_target("thread/a")));
        assert!(!capabilities
            .supports_execution_target(v2::ExecutionAction::Next, &group_target("group/a")));
    }

    #[test]
    fn incompatible_schema_reports_both_supported_and_discovered_ranges() {
        let capabilities = v2::Capabilities {
            api_version: "v2".to_string(),
            schema_version: "3.1.0".to_string(),
            server_instance_id: "server".to_string(),
            ..Default::default()
        };
        let error = capabilities.validate_for_tui().unwrap_err().to_string();
        assert!(error.contains(env!("CARGO_PKG_VERSION")));
        assert!(error.contains(TUI_API_COMPATIBILITY));
        assert!(error.contains("3.1.0"));
        assert!(error.contains("compatible paired release"));
    }
}
