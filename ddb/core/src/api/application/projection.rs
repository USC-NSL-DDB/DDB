use std::collections::HashSet;

use ddb_api_extension::ExtensionStateSnapshot;
use ddb_api_types::v2::{
    target, ApiLimits, BackendDescriptor, BackendKind, Breakpoint, BreakpointFeature,
    BreakpointSpec, Capabilities, ExecutionAction, ExecutionActionCapability, ExecutionScopeKind,
    ExecutionState, ExtensionDescriptor, ExtensionState, Frame, FrameworkDescriptor, Group,
    OperationKind, OutputStreamKind, PendingCommand, PermissionScope, Process, ResourceKind, Scope,
    ScopeKind, Session, SessionStatus, SourceBreakpointLocation, SourceLocation, StateEventKind,
    SubBreakpoint, Target, Thread, ThreadState, TransportEndpoint, Variable,
};
use prost::Message;
use sha2::{Digest, Sha256};

use crate::{
    api::read_model::{ApiPendingCommandView, GroupView, ProcessView, SessionView, ThreadView},
    common::{
        config::{DebuggerBackendKind, Framework},
        Config,
    },
    state::{BreakpointSnapshot, SubBreakpointSnapshot},
};

use super::debugger_reads::{DecodedFrame, DecodedVariable};
use super::{ApplicationError, OpaqueIdRegistry, ResourceCatalog, ResourceIdKind};

pub(crate) struct ProjectionContext<'a> {
    ids: &'a OpaqueIdRegistry,
    resources: &'a ResourceCatalog,
    config: &'a Config,
}

impl<'a> ProjectionContext<'a> {
    pub(crate) fn new(
        ids: &'a OpaqueIdRegistry,
        resources: &'a ResourceCatalog,
        config: &'a Config,
    ) -> Self {
        Self {
            ids,
            resources,
            config,
        }
    }

    pub(crate) fn session(&self, view: &SessionView) -> Result<Session, ApplicationError> {
        let session_id = self.ids.encode(ResourceIdKind::Session, view.sid)?;
        let created_at = self
            .resources
            .observe(ResourceIdKind::Session, view.sid)?
            .created_at;
        let group_id = view
            .group
            .valid
            .then(|| self.ids.encode(ResourceIdKind::Group, view.group.id))
            .transpose()?;
        let selected_thread_id = view
            .selected_thread_id
            .map(|id| self.ids.encode(ResourceIdKind::Thread, id))
            .transpose()?;
        let status = match view.status.as_str() {
            _ if view.starting => SessionStatus::Starting,
            "OFF" => SessionStatus::Exited,
            "ON" if view.all_threads_stopped => SessionStatus::Stopped,
            "ON" => SessionStatus::Running,
            _ => SessionStatus::Failed,
        };
        let status_detail = view
            .in_custom_context
            .then(|| "ddb custom distributed context is active".to_string());
        let mut session = Session {
            session_id,
            display_name: if view.alias == "UNKNOWN" {
                view.tag.clone()
            } else {
                format!("{} ({})", view.alias, view.tag)
            },
            backend: Some(self.backend()),
            status: status as i32,
            status_detail,
            process_id: None,
            group_id,
            selected_thread_id,
            created_at: Some(created_at),
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Session,
            view.sid,
            &session.encode_to_vec(),
        )?;
        session.revision = metadata.revision;
        Ok(session)
    }

    pub(crate) fn thread(&self, view: &ThreadView) -> Result<Thread, ApplicationError> {
        let thread_id = self.ids.encode(ResourceIdKind::Thread, view.global_id)?;
        let session_id = self.ids.encode(ResourceIdKind::Session, view.session_id)?;
        let process_id = view
            .process_id
            .map(|id| self.ids.encode(ResourceIdKind::Process, id))
            .transpose()?;
        let group_id = view
            .group_id
            .map(|id| self.ids.encode(ResourceIdKind::Group, id))
            .transpose()?;
        let state = match view.status {
            "running" => ThreadState::Running,
            "stopped" => ThreadState::Stopped,
            _ => ThreadState::Unavailable,
        };
        let mut thread = Thread {
            thread_id,
            session_id,
            process_id,
            group_id,
            name: None,
            backend_thread_id: Some(view.backend_thread_id.clone()),
            state: state as i32,
            selected: view.selected,
            location: view.location.as_ref().and_then(source_location),
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Thread,
            view.global_id,
            &thread.encode_to_vec(),
        )?;
        thread.revision = metadata.revision;
        Ok(thread)
    }

    pub(crate) fn process(&self, view: &ProcessView) -> Result<Process, ApplicationError> {
        let mut process = Process {
            process_id: self.ids.encode(ResourceIdKind::Process, view.global_id)?,
            session_id: self.ids.encode(ResourceIdKind::Session, view.session_id)?,
            group_id: view
                .group_id
                .map(|id| self.ids.encode(ResourceIdKind::Group, id))
                .transpose()?,
            name: None,
            system_process_id: view.system_process_id.map(|pid| pid.to_string()),
            executable: None,
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Process,
            view.global_id,
            &process.encode_to_vec(),
        )?;
        process.revision = metadata.revision;
        Ok(process)
    }

    pub(crate) fn execution_state(
        &self,
        target: Target,
        threads: &[ThreadView],
    ) -> Result<Option<(String, ExecutionState)>, ApplicationError> {
        let mut threads = threads
            .iter()
            .filter(|thread| thread.status != "unavailable")
            .collect::<Vec<_>>();
        if threads.is_empty() {
            return Ok(None);
        }
        threads.sort_unstable_by_key(|thread| thread.global_id);
        let running = threads.iter().any(|thread| thread.status == "running");
        let target_key = format!("{:x}", Sha256::digest(target.encode_to_vec()));
        let mut state = ExecutionState {
            execution_state_id: self
                .ids
                .encode(ResourceIdKind::ExecutionState, &target_key)?,
            target: Some(target),
            running,
            stop_reason: None,
            location: (!running)
                .then(|| {
                    threads
                        .iter()
                        .find(|thread| thread.selected && thread.location.is_some())
                        .or_else(|| threads.iter().find(|thread| thread.location.is_some()))
                        .and_then(|thread| thread.location.as_ref())
                        .and_then(source_location)
                })
                .flatten(),
            revision: 0,
        };
        let mut version = state.encode_to_vec();
        for thread in threads {
            version.extend_from_slice(&thread.global_id.to_le_bytes());
            version.extend_from_slice(&thread.execution_revision.to_le_bytes());
            version.extend_from_slice(thread.status.as_bytes());
            version.push(0);
        }
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::ExecutionState,
            &target_key,
            &version,
        )?;
        state.revision = metadata.revision;
        Ok(Some((target_key, state)))
    }

    pub(crate) fn pending_command(
        &self,
        view: &ApiPendingCommandView,
    ) -> Result<PendingCommand, ApplicationError> {
        let internal_id = pending_command_internal_id(view.sid, view.token);
        let operation_kind = view
            .operation_kind
            .and_then(|kind| i32::try_from(kind).ok())
            .and_then(|kind| OperationKind::try_from(kind).ok())
            .unwrap_or(OperationKind::Unspecified);
        let command = PendingCommand {
            pending_command_id: self
                .ids
                .encode(ResourceIdKind::PendingCommand, &internal_id)?,
            session_id: self.ids.encode(ResourceIdKind::Session, view.sid)?,
            operation_id: view.operation_id.clone(),
            kind: operation_kind as i32,
            enqueued_at: Some(super::context::system_time_to_timestamp(view.enqueued_at)),
            running: view.running,
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::PendingCommand,
            &internal_id,
            &command.encode_to_vec(),
        )?;
        let mut command = command;
        command.revision = metadata.revision;
        Ok(command)
    }

    pub(crate) fn frame(
        &self,
        view: &DecodedFrame,
        global_thread_id: u64,
        execution_revision: u64,
    ) -> Result<Frame, ApplicationError> {
        let thread_id = self.ids.encode(ResourceIdKind::Thread, global_thread_id)?;
        let frame_key = format!("{global_thread_id}:{execution_revision}:{}", view.level);
        let frame_id = self.ids.encode(ResourceIdKind::Frame, frame_key)?;
        Ok(Frame {
            frame_id,
            thread_id,
            level: view.level,
            function_name: view.function_name.clone(),
            location: Some(SourceLocation {
                source_reference: None,
                path: view.path.clone(),
                line: view.line,
                column: 0,
                address: view.address.clone(),
                function_name: view.function_name.clone(),
            }),
            module: view.module.clone(),
            synthetic: false,
        })
    }

    pub(crate) fn locals_scope(&self, frame_key: &str) -> Result<Scope, ApplicationError> {
        let frame_id = self.ids.encode(ResourceIdKind::Frame, frame_key)?;
        let scope_id = self
            .ids
            .encode(ResourceIdKind::Scope, format!("{frame_key}:locals"))?;
        Ok(Scope {
            scope_id,
            frame_id,
            kind: ScopeKind::Locals as i32,
            name: "Locals and arguments".to_string(),
            expensive: false,
            variable_count: None,
        })
    }

    pub(crate) fn variable(
        &self,
        view: &DecodedVariable,
        internal_id: &str,
        evaluate_name: Option<String>,
        presentation_hint: Option<String>,
    ) -> Result<Variable, ApplicationError> {
        let variable_id = self.ids.encode(ResourceIdKind::Variable, internal_id)?;
        Ok(Variable {
            variable_id,
            name: view.name.clone(),
            value: view.value.clone(),
            type_name: view.type_name.clone(),
            has_children: view.child_count.is_some_and(|count| count > 0),
            child_count: view.child_count,
            evaluate_name,
            address: None,
            presentation_hint,
        })
    }

    pub(crate) fn group(
        &self,
        view: &GroupView,
        selected_session: Option<u64>,
    ) -> Result<Group, ApplicationError> {
        let group_id = self.ids.encode(ResourceIdKind::Group, view.id)?;
        let session_ids = view
            .sids
            .iter()
            .map(|sid| self.ids.encode(ResourceIdKind::Session, sid))
            .collect::<Result<Vec<_>, _>>()?;
        let mut group = Group {
            group_id,
            display_name: view.alias.clone(),
            session_ids,
            selected: selected_session.is_some_and(|sid| view.sids.contains(&sid)),
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Group,
            view.id,
            &group.encode_to_vec(),
        )?;
        group.revision = metadata.revision;
        Ok(group)
    }

    pub(crate) fn breakpoint(
        &self,
        snapshot: &BreakpointSnapshot,
    ) -> Result<Breakpoint, ApplicationError> {
        let breakpoint_id = self.ids.encode(ResourceIdKind::Breakpoint, snapshot.id)?;
        let line = u32::try_from(snapshot.location.line).map_err(|_| {
            ApplicationError::backend("backend breakpoint line exceeds the public line range")
        })?;

        let mut target_keys = HashSet::new();
        let mut targets = Vec::new();
        let mut sub_breakpoints = Vec::new();
        let mut pending = false;
        for sub in &snapshot.subbkpts {
            match sub {
                SubBreakpointSnapshot::Session {
                    id, target_session, ..
                } => {
                    let session_id = self.ids.encode(ResourceIdKind::Session, target_session)?;
                    if target_keys.insert(format!("session:{session_id}")) {
                        targets.push(Target {
                            selector: Some(target::Selector::Session(
                                ddb_api_types::v2::SessionTarget {
                                    session_id: session_id.clone(),
                                },
                            )),
                        });
                    }
                    let internal_sub_id = format!("{}:{id}", snapshot.id);
                    let sub_breakpoint_id = self
                        .ids
                        .encode(ResourceIdKind::SubBreakpoint, &internal_sub_id)?;
                    let mut sub_breakpoint = SubBreakpoint {
                        sub_breakpoint_id,
                        session_id,
                        inherited_from_group_id: None,
                        location: Some(SourceLocation {
                            source_reference: None,
                            path: Some(snapshot.location.src.clone()),
                            line,
                            column: 0,
                            address: None,
                            function_name: None,
                        }),
                        verified: true,
                        message: None,
                        hit_count: 0,
                        revision: 0,
                    };
                    let sub_metadata = self.resources.observe_versioned(
                        ResourceIdKind::SubBreakpoint,
                        &internal_sub_id,
                        &sub_breakpoint.encode_to_vec(),
                    )?;
                    sub_breakpoint.revision = sub_metadata.revision;
                    sub_breakpoints.push(sub_breakpoint);
                }
                SubBreakpointSnapshot::Group {
                    target_group,
                    active_sessions,
                    ..
                } => {
                    let group_id = self.ids.encode(ResourceIdKind::Group, target_group)?;
                    if target_keys.insert(format!("group:{group_id}")) {
                        targets.push(Target {
                            selector: Some(target::Selector::Group(
                                ddb_api_types::v2::GroupTarget { group_id },
                            )),
                        });
                    }
                    pending |= *active_sessions == 0;
                }
            }
        }
        let target = match targets.len() {
            0 => Target {
                selector: Some(target::Selector::Broadcast(
                    ddb_api_types::v2::BroadcastTarget {},
                )),
            },
            1 => targets.pop().expect("one target exists"),
            _ => Target {
                selector: Some(target::Selector::Multiple(
                    ddb_api_types::v2::MultipleTarget { targets },
                )),
            },
        };
        let verified = !sub_breakpoints.is_empty() && !pending;
        let mut breakpoint = Breakpoint {
            breakpoint_id,
            target: Some(target),
            spec: Some(BreakpointSpec {
                location: Some(ddb_api_types::v2::breakpoint_spec::Location::Source(
                    SourceBreakpointLocation {
                        source: snapshot.location.src.clone(),
                        line,
                        column: 0,
                    },
                )),
                enabled: Some(snapshot.enabled),
                condition: snapshot.condition.clone(),
                ignore_count: None,
                temporary: snapshot.temporary,
                hardware: snapshot.hardware,
            }),
            verified,
            pending,
            hit_count: snapshot.times,
            message: pending.then(|| "waiting for an eligible group session".to_string()),
            sub_breakpoints,
            revision: 0,
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Breakpoint,
            snapshot.id,
            &breakpoint.encode_to_vec(),
        )?;
        breakpoint.revision = metadata.revision;
        Ok(breakpoint)
    }

    pub(crate) fn extension_state(
        &self,
        state: &ExtensionStateSnapshot,
    ) -> Result<ExtensionState, ApplicationError> {
        let mut extension_state = ExtensionState {
            extension_state_id: self
                .ids
                .encode(ResourceIdKind::Extension, &state.extension_id)?,
            extension_id: state.extension_id.clone(),
            revision: 0,
            payloads: state.payloads.clone(),
        };
        let metadata = self.resources.observe_versioned(
            ResourceIdKind::Extension,
            &state.extension_id,
            &extension_state.encode_to_vec(),
        )?;
        extension_state.revision = metadata.revision;
        Ok(extension_state)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capabilities(
        &self,
        capabilities_id: String,
        server_instance_id: &str,
        transports: Vec<TransportEndpoint>,
        limits: ApiLimits,
        extensions: Vec<ExtensionDescriptor>,
        supported_operations: Vec<OperationKind>,
        execution_actions: Vec<ExecutionAction>,
        state_event_kinds: Vec<StateEventKind>,
        output_stream_kinds: Vec<OutputStreamKind>,
        authentication_mode: String,
        revision: u64,
    ) -> Capabilities {
        let mut breakpoint_features = vec![
            BreakpointFeature::Source as i32,
            BreakpointFeature::Condition as i32,
            BreakpointFeature::Temporary as i32,
        ];
        if !matches!(self.config.conf.debugger.backend, DebuggerBackendKind::Lldb) {
            breakpoint_features.push(BreakpointFeature::Hardware as i32);
        }
        breakpoint_features.extend([
            BreakpointFeature::EnableDisable as i32,
            BreakpointFeature::Distributed as i32,
            BreakpointFeature::GroupInheritance as i32,
        ]);

        let execution_action_capabilities = execution_actions
            .iter()
            .map(|action| {
                let scopes = match action {
                    ExecutionAction::Continue | ExecutionAction::Interrupt => vec![
                        ExecutionScopeKind::Thread,
                        ExecutionScopeKind::Session,
                        ExecutionScopeKind::Group,
                        ExecutionScopeKind::Fanout,
                    ],
                    ExecutionAction::Next | ExecutionAction::StepIn | ExecutionAction::StepOut => {
                        vec![ExecutionScopeKind::Thread]
                    }
                    ExecutionAction::Jump | ExecutionAction::Signal => {
                        vec![ExecutionScopeKind::Thread, ExecutionScopeKind::Session]
                    }
                    ExecutionAction::ReverseContinue
                    | ExecutionAction::ReverseNext
                    | ExecutionAction::ReverseStepIn
                    | ExecutionAction::Unspecified => Vec::new(),
                };
                ExecutionActionCapability {
                    action: *action as i32,
                    scopes: scopes.into_iter().map(|scope| scope as i32).collect(),
                }
            })
            .collect();

        Capabilities {
            capabilities_id,
            api_version: "v2".to_string(),
            schema_version: "2.0.0-draft.3".to_string(),
            server_instance_id: server_instance_id.to_string(),
            transports,
            backends: vec![self.backend()],
            frameworks: self.framework().into_iter().collect(),
            supported_resources: vec![
                ResourceKind::Session as i32,
                ResourceKind::Group as i32,
                ResourceKind::Process as i32,
                ResourceKind::Thread as i32,
                ResourceKind::PendingCommand as i32,
                ResourceKind::ExecutionState as i32,
                ResourceKind::Selection as i32,
                ResourceKind::Breakpoint as i32,
                ResourceKind::Operation as i32,
                ResourceKind::Capabilities as i32,
                ResourceKind::ExtensionState as i32,
            ],
            supported_operations: supported_operations
                .into_iter()
                .map(|kind| kind as i32)
                .collect(),
            execution_actions: execution_actions
                .into_iter()
                .map(|action| action as i32)
                .collect(),
            breakpoint_features,
            state_event_kinds: state_event_kinds
                .into_iter()
                .map(|kind| kind as i32)
                .collect(),
            output_stream_kinds: output_stream_kinds
                .into_iter()
                .map(|kind| kind as i32)
                .collect(),
            cancellable_operation_kinds: Vec::new(),
            ddb_features: vec![
                "distributed_backtrace".to_string(),
                "multi_target_routing".to_string(),
                "group_breakpoints".to_string(),
            ],
            limits: Some(limits),
            authentication_mode,
            permission_scopes: vec![
                PermissionScope::Read as i32,
                PermissionScope::Control as i32,
                PermissionScope::Admin as i32,
            ],
            extensions,
            deprecations: Vec::new(),
            revision,
            execution_action_capabilities,
        }
    }

    fn backend(&self) -> BackendDescriptor {
        let kind = match self.config.conf.debugger.backend {
            DebuggerBackendKind::Gdb => BackendKind::Gdb,
            DebuggerBackendKind::Lldb => BackendKind::Lldb,
            DebuggerBackendKind::Mock => BackendKind::Mock,
            DebuggerBackendKind::Unknown => BackendKind::Other,
        };
        BackendDescriptor {
            kind: kind as i32,
            version: None,
            capability_namespace: Some(
                match kind {
                    BackendKind::Gdb => "ddb.backend.gdb",
                    BackendKind::Lldb => "ddb.backend.lldb",
                    BackendKind::Mock => "ddb.backend.mock",
                    BackendKind::Other | BackendKind::Unspecified => "ddb.backend.other",
                }
                .to_string(),
            ),
        }
    }

    fn framework(&self) -> Option<FrameworkDescriptor> {
        let (framework_id, display_name) = match self.config.framework {
            Framework::Nu => ("ddb.framework.nu", "Nu"),
            Framework::Quicksand => ("ddb.framework.quicksand", "Quicksand"),
            Framework::ServiceWeaverKube => ("ddb.framework.service_weaver", "Service Weaver"),
            Framework::GRPC => ("ddb.framework.grpc", "gRPC"),
            Framework::Unspecified => return None,
        };
        Some(FrameworkDescriptor {
            framework_id: framework_id.to_string(),
            display_name: display_name.to_string(),
            version: None,
        })
    }
}

fn source_location(location: &crate::state::ThreadLocation) -> Option<SourceLocation> {
    let line = location
        .line
        .map(u32::try_from)
        .transpose()
        .ok()?
        .unwrap_or(0);
    let column = location
        .column
        .map(u32::try_from)
        .transpose()
        .ok()?
        .unwrap_or(0);
    Some(SourceLocation {
        source_reference: None,
        path: location.path.clone(),
        line,
        column,
        address: location.address.clone(),
        function_name: location.function_name.clone(),
    })
}

pub(super) fn pending_command_internal_id(sid: u64, token: u64) -> String {
    format!("{sid}:{token}")
}

pub(crate) fn collection_revision<T: Message>(items: &[T]) -> u64 {
    let mut digest = Sha256::new();
    for item in items {
        let bytes = item.encode_to_vec();
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let hash = digest.finalize();
    u64::from_le_bytes(hash[..8].try_into().expect("SHA-256 prefix is eight bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::read_model::SessionGroupView;

    #[test]
    fn preactivation_session_projects_as_starting() {
        let ids = OpaqueIdRegistry::new(16);
        let resources = ResourceCatalog::new(16, 128, 1_024);
        let config = Config::default();
        let projection = ProjectionContext::new(&ids, &resources, &config);
        let session = projection
            .session(&SessionView {
                sid: 7,
                tag: "starting".to_string(),
                alias: "UNKNOWN".to_string(),
                status: "OFF".to_string(),
                starting: true,
                group: SessionGroupView {
                    valid: false,
                    id: 0,
                    hash: "UNKNOWN".to_string(),
                },
                selected_thread_id: None,
                in_custom_context: false,
                all_threads_stopped: true,
            })
            .unwrap();

        assert_eq!(
            SessionStatus::try_from(session.status).unwrap(),
            SessionStatus::Starting
        );
    }
}
