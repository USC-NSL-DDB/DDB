use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

use uuid::Uuid;

use super::ApplicationError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ResourceIdKind {
    Session,
    Group,
    Process,
    Thread,
    Frame,
    Scope,
    Variable,
    Source,
    ExecutionState,
    Breakpoint,
    SubBreakpoint,
    PendingCommand,
    Extension,
    Selection,
    Capabilities,
}

impl ResourceIdKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Session => "ses",
            Self::Group => "grp",
            Self::Process => "prc",
            Self::Thread => "thr",
            Self::Frame => "frm",
            Self::Scope => "scp",
            Self::Variable => "var",
            Self::Source => "src",
            Self::ExecutionState => "exe",
            Self::Breakpoint => "bpt",
            Self::SubBreakpoint => "sbp",
            Self::PendingCommand => "cmd",
            Self::Extension => "ext",
            Self::Selection => "sel",
            Self::Capabilities => "cap",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Group => "group",
            Self::Process => "process",
            Self::Thread => "thread",
            Self::Frame => "frame",
            Self::Scope => "scope",
            Self::Variable => "variable",
            Self::Source => "source",
            Self::ExecutionState => "execution state",
            Self::Breakpoint => "breakpoint",
            Self::SubBreakpoint => "sub-breakpoint",
            Self::PendingCommand => "pending command",
            Self::Extension => "extension",
            Self::Selection => "selection",
            Self::Capabilities => "capabilities",
        }
    }
}

#[derive(Default)]
struct RegistryState {
    forward: HashMap<(ResourceIdKind, String), String>,
    reverse: HashMap<String, (ResourceIdKind, String)>,
}

/// Per-server mapping that keeps domain IDs out of the public contract.
pub(crate) struct OpaqueIdRegistry {
    max_entries: usize,
    state: Mutex<RegistryState>,
}

impl OpaqueIdRegistry {
    pub(crate) fn new(max_entries: usize) -> Self {
        assert!(max_entries > 0, "opaque ID registry must be bounded");
        Self {
            max_entries,
            state: Mutex::new(RegistryState::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, RegistryState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn encode(
        &self,
        kind: ResourceIdKind,
        internal_id: impl ToString,
    ) -> Result<String, ApplicationError> {
        let internal_id = internal_id.to_string();
        let mut state = self.state();
        if let Some(public_id) = state.forward.get(&(kind, internal_id.clone())) {
            return Ok(public_id.clone());
        }
        if state.forward.len() >= self.max_entries {
            return Err(ApplicationError::resource_exhausted(
                "public resource identity capacity is exhausted",
            ));
        }

        let public_id = format!("{}_{}", kind.prefix(), Uuid::new_v4().simple());
        state
            .forward
            .insert((kind, internal_id.clone()), public_id.clone());
        state.reverse.insert(public_id.clone(), (kind, internal_id));
        Ok(public_id)
    }

    pub(crate) fn decode(
        &self,
        kind: ResourceIdKind,
        public_id: &str,
    ) -> Result<String, ApplicationError> {
        if public_id.is_empty() {
            return Err(ApplicationError::invalid(
                format!("{}_id", kind.display_name().replace('-', "_")),
                "must not be empty",
            ));
        }
        self.state()
            .reverse
            .get(public_id)
            .filter(|(found_kind, _)| *found_kind == kind)
            .map(|(_, internal_id)| internal_id.clone())
            .ok_or_else(|| ApplicationError::not_found(kind.display_name()))
    }

    #[cfg(test)]
    fn remove(&self, kind: ResourceIdKind, internal_id: impl ToString) {
        let internal_id = internal_id.to_string();
        let mut state = self.state();
        if let Some(public_id) = state.forward.remove(&(kind, internal_id)) {
            state.reverse.remove(&public_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use ddb_api_types::v2::DdbErrorCode;

    use super::*;

    #[test]
    fn public_ids_are_stable_opaque_and_kind_checked() {
        let registry = OpaqueIdRegistry::new(4);
        let first = registry.encode(ResourceIdKind::Session, 42).unwrap();
        assert_eq!(first, registry.encode(ResourceIdKind::Session, 42).unwrap());
        assert!(first.starts_with("ses_"));
        assert_ne!(first, "ses_42");
        assert_eq!(
            registry.decode(ResourceIdKind::Session, &first).unwrap(),
            "42"
        );
        assert_eq!(
            registry
                .decode(ResourceIdKind::Thread, &first)
                .unwrap_err()
                .code(),
            DdbErrorCode::NotFound
        );
    }

    #[test]
    fn registry_rejects_growth_past_its_bound_and_reclaims_removed_ids() {
        let registry = OpaqueIdRegistry::new(1);
        registry.encode(ResourceIdKind::Session, 1).unwrap();
        assert_eq!(
            registry
                .encode(ResourceIdKind::Session, 2)
                .unwrap_err()
                .code(),
            DdbErrorCode::ResourceExhausted
        );
        registry.remove(ResourceIdKind::Session, 1);
        registry.encode(ResourceIdKind::Session, 2).unwrap();
    }
}
