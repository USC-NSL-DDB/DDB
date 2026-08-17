use std::{collections::HashSet, sync::Arc, time::Duration};

use ddb_api_extension::{
    ExtensionInvocation, ExtensionRegistry, InvocationError, ProviderErrorKind,
};
use ddb_api_types::v2::{
    breakpoint_spec, dynamic_value, operation_result, resource_upsert, state_event, target,
    Breakpoint, BreakpointSpec, CancelOperationRequest, CreateBreakpointRequest, DdbErrorCode,
    DeleteBreakpointRequest, DistributedBacktraceResult, DistributedBoundaryKind, DistributedFrame,
    DynamicList, DynamicObject, DynamicValue, Empty, EvaluateRequest, EvaluationContext,
    EvaluationResult, ExecuteRawCommandRequest, ExecuteRequest, ExecutionAction, Frame,
    InvokeExtensionActionRequest, InvokeExtensionActionResult, Operation,
    OperationAdmissionResponse, OperationKind, OperationResult, PermissionScope, Preconditions,
    RawCommandDialect, RawCommandResult, RequestContext, ResourceDeleted, ResourceKind,
    ResourceUpsert, RunDistributedBacktraceRequest, SelectThreadRequest, SessionTarget,
    ShutdownRequest, SourceLocation, StateEventKind, Target as PublicTarget, TargetFailure,
    TargetOutcome, UpdateBreakpointRequest,
};
use prost::Message;
use tracing::warn;

use crate::{
    cmd_flow::{
        breakpoint::breakpoint_insert_command,
        input::ParsedInputCmd,
        router::{
            CommandFanoutReport, SessionCommandFailure, SessionCommandFailureKind,
            Target as CommandTarget,
        },
        CommandOutcome,
    },
    common::config::DebuggerBackendKind,
    debugger::protocol::{Dict, Value as DebuggerValue},
    shutdown::ShutdownCause,
    state::{BkptLoc, BreakpointProperties},
};

use super::service::StopFrameKey;
use super::{
    ApplicationError, CommandPortError, DdbApplicationService, PrincipalContext, RequestScope,
    ResolvedTarget, ResourceIdKind, StateChange, StateEventContext, TargetPurpose, TargetResolver,
};

const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_SIGNAL_BYTES: usize = 128;
const MAX_DYNAMIC_NODES: usize = 2_048;
const MAX_RETAINED_RESULT_BYTES: usize = 48 * 1024;
const DEFAULT_DISTRIBUTED_FRAMES: usize = 256;
const MAX_DISTRIBUTED_FRAMES: usize = 4_096;

#[derive(Clone)]
enum CompletionProjection {
    NoContent,
    Selection,
    Evaluation {
        frame: Option<StopFrameKey>,
    },
    CreatedBreakpoint,
    UpdatedBreakpoint(u64),
    DeletedBreakpoint {
        internal_id: u64,
        public_id: String,
        context: StateEventContext,
    },
    RawCommand,
    DistributedBacktrace {
        max_frames: usize,
    },
}

struct CommandOperationTask {
    operation_id: String,
    request_id: String,
    target: CommandTarget,
    session_ids: Vec<u64>,
    command: String,
    kind: OperationKind,
    projection: CompletionProjection,
}

struct ExtensionOperationTask {
    operation_id: String,
    request_id: String,
    session_ids: Vec<u64>,
    registry: Arc<ExtensionRegistry>,
    invocation: ExtensionInvocation,
}

impl CompletionProjection {
    fn frame_guard(&self) -> Option<&StopFrameKey> {
        match self {
            Self::Evaluation { frame } => frame.as_ref(),
            _ => None,
        }
    }

    fn requires_concrete_target_completions(&self) -> bool {
        matches!(self, Self::NoContent | Self::RawCommand)
    }
}

impl DdbApplicationService {
    pub(crate) async fn execute(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: ExecuteRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        let command = execution_command(&request)?;
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        validate_execution_target(request.action, &resolved.command)?;
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::Execute,
            resolved,
            command,
            CompletionProjection::NoContent,
        )
    }

    pub(crate) async fn select_thread(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: SelectThreadRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        let CommandTarget::Thread(thread_id) = resolved.command else {
            return Err(ApplicationError::invalid(
                "target",
                "thread selection requires a thread or current-thread target",
            ));
        };
        let command = format!("-thread-select {}", thread_id.value());
        let resolved = ResolvedTarget {
            command: CommandTarget::Thread(thread_id),
            ..resolved
        };
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::SelectThread,
            resolved,
            command,
            CompletionProjection::Selection,
        )
    }

    pub(crate) async fn evaluate(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: EvaluateRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        require_nonempty_bounded("expression", &request.expression, MAX_COMMAND_BYTES)?;
        let evaluation_context =
            EvaluationContext::try_from(request.evaluation_context).map_err(|_| {
                ApplicationError::invalid(
                    "evaluation_context",
                    format!("unknown evaluation context {}", request.evaluation_context),
                )
            })?;
        if evaluation_context == EvaluationContext::Unspecified {
            return Err(ApplicationError::invalid(
                "evaluation_context",
                "UNSPECIFIED is not an evaluation context",
            ));
        }
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let mut resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        if resolved.resolved_target_count != 1 {
            return Err(ApplicationError::invalid(
                "target",
                "evaluation must resolve to exactly one debugger session",
            ));
        }
        let frame = match request.frame_id.as_deref() {
            Some(frame_id) => Some(self.current_frame(frame_id).await?),
            None => None,
        };
        if let Some(frame) = frame.as_ref() {
            let frame_session_id = self
                .queries
                .thread_session_id(frame.global_thread_id)
                .ok_or_else(|| ApplicationError::not_found("frame thread"))?;
            if resolved.session_ids.as_slice() != [frame_session_id] {
                return Err(ApplicationError::invalid(
                    "target",
                    "must resolve to the debugger session that owns frame_id",
                ));
            }
            if matches!(
                &resolved.command,
                CommandTarget::Thread(thread) if thread.value() != frame.global_thread_id
            ) {
                return Err(ApplicationError::invalid(
                    "target",
                    "thread target does not own frame_id",
                ));
            }
            resolved.command =
                CommandTarget::Thread(crate::state::GlobalThreadId::new(frame.global_thread_id));
        }
        let frame_option = frame
            .as_ref()
            .map(|frame| {
                format!(
                    " --thread {} --frame {}",
                    frame.global_thread_id, frame.level
                )
            })
            .unwrap_or_default();
        let command = format!(
            "-data-evaluate-expression{frame_option} {}",
            quote(&request.expression)
        );
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::Evaluate,
            resolved,
            command,
            CompletionProjection::Evaluation { frame },
        )
    }

    pub(crate) async fn create_breakpoint(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: CreateBreakpointRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        let spec = request
            .breakpoint
            .as_ref()
            .ok_or_else(|| ApplicationError::invalid("breakpoint", "is required"))?;
        let (location, properties) = breakpoint_definition(spec)?;
        if properties.hardware
            && matches!(self.config.conf.debugger.backend, DebuggerBackendKind::Lldb)
        {
            return Err(ApplicationError::new(
                DdbErrorCode::Unsupported,
                "hardware breakpoints are unavailable through the configured LLDB backend",
            )
            .requiring("breakpoints.hardware"));
        }
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Breakpoint)
            .await?;
        let command = breakpoint_insert_command(&location, &properties);
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::CreateBreakpoint,
            resolved,
            command,
            CompletionProjection::CreatedBreakpoint,
        )
    }

    pub(crate) async fn update_breakpoint(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: UpdateBreakpointRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        let internal_id = self.decode_breakpoint_id(&request.breakpoint_id)?;
        let snapshot = self
            .queries
            .breakpoints()
            .into_iter()
            .find(|breakpoint| breakpoint.id == internal_id)
            .ok_or_else(|| ApplicationError::not_found("breakpoint"))?;
        let current = self.projection().breakpoint(&snapshot)?;
        self.validate_preconditions(
            request.preconditions.as_ref(),
            Some((&request.breakpoint_id, current.revision)),
        )?;
        let spec = request
            .breakpoint
            .as_ref()
            .ok_or_else(|| ApplicationError::invalid("breakpoint", "is required"))?;
        let mask = request
            .update_mask
            .as_ref()
            .ok_or_else(|| ApplicationError::invalid("update_mask", "is required"))?;
        if mask.paths.is_empty() {
            return Err(ApplicationError::invalid(
                "update_mask",
                "must not be empty",
            ));
        }
        let mut fields = HashSet::new();
        for field in &mask.paths {
            if !fields.insert(field.as_str()) {
                return Err(ApplicationError::invalid(
                    "update_mask",
                    format!("contains duplicate field {field:?}"),
                ));
            }
            if !matches!(field.as_str(), "enabled" | "condition") {
                return Err(ApplicationError::new(
                    DdbErrorCode::Unsupported,
                    format!(
                        "breakpoint field {field:?} cannot be updated; supported fields are enabled and condition"
                    ),
                )
                .requiring(format!("breakpoints.update.{field}")));
            }
        }
        let mut command = format!("-break-update {internal_id}");
        if fields.contains("enabled") {
            let enabled = spec.enabled.ok_or_else(|| {
                ApplicationError::invalid(
                    "breakpoint.enabled",
                    "must be present when update_mask contains enabled",
                )
            })?;
            command.push_str(if enabled {
                " --enabled true"
            } else {
                " --enabled false"
            });
        }
        if fields.contains("condition") {
            match spec.condition.as_deref() {
                Some(condition) => {
                    require_nonempty_bounded("breakpoint.condition", condition, MAX_COMMAND_BYTES)?;
                    command.push_str(" --condition ");
                    command.push_str(&quote(condition));
                }
                None => command.push_str(" --clear-condition"),
            }
        }
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Breakpoint)
            .await?;
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::UpdateBreakpoint,
            resolved,
            command,
            CompletionProjection::UpdatedBreakpoint(internal_id),
        )
    }

    pub(crate) async fn delete_breakpoint(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: DeleteBreakpointRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        let internal_id = self.decode_breakpoint_id(&request.breakpoint_id)?;
        let snapshot = self
            .queries
            .breakpoints()
            .into_iter()
            .find(|breakpoint| breakpoint.id == internal_id)
            .ok_or_else(|| ApplicationError::not_found("breakpoint"))?;
        let current = self.projection().breakpoint(&snapshot)?;
        let deletion_context = StateEventContext::from_resource(
            &resource_upsert::Resource::Breakpoint(current.clone()),
        );
        self.validate_preconditions(
            request.preconditions.as_ref(),
            Some((&request.breakpoint_id, current.revision)),
        )?;
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Breakpoint)
            .await?;
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::DeleteBreakpoint,
            resolved,
            format!("-break-delete {internal_id}"),
            CompletionProjection::DeletedBreakpoint {
                internal_id,
                public_id: request.breakpoint_id,
                context: deletion_context,
            },
        )
    }

    pub(crate) async fn execute_raw_command(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: ExecuteRawCommandRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        require_nonempty_bounded("command", &request.command, MAX_COMMAND_BYTES)?;
        let dialect = RawCommandDialect::try_from(request.dialect).map_err(|_| {
            ApplicationError::invalid(
                "dialect",
                format!("unknown raw dialect {}", request.dialect),
            )
        })?;
        if dialect != RawCommandDialect::GdbMi {
            return Err(ApplicationError::new(
                DdbErrorCode::Unsupported,
                "the public raw-command escape hatch currently accepts the DDB MI facade",
            )
            .requiring("raw_command.gdb_mi"));
        }
        let parsed: ParsedInputCmd = request.command.as_str().try_into().map_err(|_| {
            ApplicationError::invalid("command", "is not valid DDB/MI command syntax")
        })?;
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let target_purpose = if parsed.prefix == "-break-insert" {
            TargetPurpose::Breakpoint
        } else {
            TargetPurpose::Command
        };
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), target_purpose)
            .await?;
        validate_raw_command_target(&parsed, &resolved.command)?;
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::RawCommand,
            resolved,
            request.command,
            CompletionProjection::RawCommand,
        )
    }

    pub(crate) async fn run_distributed_backtrace(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: RunDistributedBacktraceRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        let max_frames = if request.max_frames == 0 {
            DEFAULT_DISTRIBUTED_FRAMES
        } else {
            request.max_frames as usize
        };
        if max_frames > MAX_DISTRIBUTED_FRAMES {
            return Err(ApplicationError::invalid(
                "max_frames",
                format!("must not exceed {MAX_DISTRIBUTED_FRAMES}"),
            ));
        }
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        if !matches!(resolved.command, CommandTarget::Thread(_)) {
            return Err(ApplicationError::invalid(
                "target",
                "distributed backtrace requires a thread or current-thread target",
            ));
        }
        scope.ensure_active()?;
        self.admit_command(
            principal,
            &scope,
            request.context.as_ref(),
            &fingerprint,
            OperationKind::DistributedBacktrace,
            resolved,
            "-bt-remote".to_string(),
            CompletionProjection::DistributedBacktrace { max_frames },
        )
    }

    pub(crate) async fn invoke_extension_action(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: InvokeExtensionActionRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }

        let registry = self.queries.extension_registry();
        let action = registry
            .action(&request.extension_id, &request.action_id)
            .cloned()
            .ok_or_else(|| {
                if registry.descriptor(&request.extension_id).is_some() {
                    ApplicationError::not_found("extension action")
                } else {
                    ApplicationError::not_found("extension")
                }
            })?;
        let required_scope = PermissionScope::try_from(action.required_scope).map_err(|_| {
            ApplicationError::new(
                DdbErrorCode::Internal,
                "registered extension action has an invalid scope",
            )
        })?;
        if !principal.allows(required_scope) {
            return Err(ApplicationError::new(
                DdbErrorCode::PermissionDenied,
                "the authenticated principal lacks the extension action scope",
            ));
        }
        let payload = request
            .payload
            .clone()
            .ok_or_else(|| ApplicationError::invalid("payload", "is required"))?;
        let target = request
            .target
            .clone()
            .ok_or_else(|| ApplicationError::invalid("target", "is required"))?;
        let mut invocation = ExtensionInvocation {
            extension_id: request.extension_id.clone(),
            action_id: request.action_id.clone(),
            payload,
            target,
        };
        registry
            .validate_invocation(&invocation)
            .map_err(extension_invocation_error)?;
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let resolved = self
            .target_resolver()
            .resolve(request.target.as_ref(), TargetPurpose::Command)
            .await?;
        invocation.target = resolved.public.clone();
        let resolved_group_ids = public_target_group_ids(&resolved.public, &self.ids)?;
        scope.ensure_active()?;
        let admission = self.operations.admit(
            request.context.as_ref(),
            principal.id(),
            None,
            scope.request_id(),
            OperationKind::ExtensionAction,
            resolved.public,
            resolved.resolved_target_count,
            &resolved.session_ids,
            &resolved_group_ids,
            false,
            &fingerprint,
        )?;
        if admission.newly_admitted {
            self.publish_operation(&admission.operation)?;
            let service = Arc::clone(self);
            let task = ExtensionOperationTask {
                operation_id: admission.operation.operation_id.clone(),
                request_id: admission.operation.request_id.clone(),
                session_ids: resolved.session_ids,
                registry,
                invocation,
            };
            tokio::spawn(async move {
                service.run_extension_operation(task).await;
            });
        }
        Ok(OperationAdmissionResponse {
            context: Some(scope.response_context(self.server_instance_id())),
            operation: Some(admission.operation),
        })
    }

    pub(crate) fn cancel_operation(
        &self,
        _principal: &PrincipalContext,
        request: CancelOperationRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let operation_id = self
            .target_resolver()
            .operation_id(request.target.as_ref())?;
        let operation = self.operations.get(&operation_id)?;
        if !operation.cancellable {
            return Err(ApplicationError::new(
                DdbErrorCode::NotCancellable,
                "operation cannot be cancelled",
            )
            .with_operation_id(operation_id));
        }
        let operation = self.operations.cancel(&operation_id)?;
        self.publish_operation(&operation)?;
        Ok(OperationAdmissionResponse {
            context: Some(scope.response_context(self.server_instance_id())),
            operation: Some(operation),
        })
    }

    pub(crate) fn shutdown_request(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        request: ShutdownRequest,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let scope = RequestScope::begin(request.context.as_ref())?;
        let fingerprint = fingerprint_without_context(&request, |request| request.context = None);
        if let Some(response) =
            self.idempotent_response(principal, &scope, request.context.as_ref(), &fingerprint)?
        {
            return Ok(response);
        }
        if request.grace_period.is_some() {
            return Err(ApplicationError::new(
                DdbErrorCode::Unsupported,
                "custom API shutdown grace periods are not available",
            )
            .requiring("admin.shutdown.grace_period"));
        }
        self.validate_preconditions(request.preconditions.as_ref(), None)?;
        let target = request
            .target
            .clone()
            .ok_or_else(|| ApplicationError::invalid("target", "is required"))?;
        if !matches!(target.selector, Some(target::Selector::Broadcast(_))) {
            return Err(ApplicationError::invalid(
                "target",
                "server shutdown requires the broadcast target",
            ));
        }
        scope.ensure_active()?;
        let admission = self.operations.admit(
            request.context.as_ref(),
            principal.id(),
            None,
            scope.request_id(),
            OperationKind::Shutdown,
            target,
            1,
            &[],
            &[],
            false,
            &fingerprint,
        )?;
        if admission.newly_admitted {
            self.publish_operation(&admission.operation)?;
            let service = Arc::clone(self);
            let operation_id = admission.operation.operation_id.clone();
            tokio::spawn(async move {
                service.finish_shutdown_operation(&operation_id);
                tokio::time::sleep(Duration::from_millis(50)).await;
                service
                    .shutdown_ctrl
                    .trigger_once(ShutdownCause::ApiRequest);
            });
        }
        Ok(OperationAdmissionResponse {
            context: Some(scope.response_context(self.server_instance_id())),
            operation: Some(admission.operation),
        })
    }

    fn target_resolver(&self) -> TargetResolver<'_> {
        TargetResolver::new(self.queries.as_ref(), &self.ids)
    }

    fn idempotent_response(
        &self,
        principal: &PrincipalContext,
        scope: &RequestScope,
        context: Option<&RequestContext>,
        fingerprint: &[u8],
    ) -> Result<Option<OperationAdmissionResponse>, ApplicationError> {
        self.operations
            .lookup_idempotent(context, principal.id(), fingerprint)
            .map(|operation| {
                operation.map(|operation| OperationAdmissionResponse {
                    context: Some(scope.response_context(self.server_instance_id())),
                    operation: Some(operation),
                })
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_command(
        self: &Arc<Self>,
        principal: &PrincipalContext,
        scope: &RequestScope,
        context: Option<&RequestContext>,
        fingerprint: &[u8],
        kind: OperationKind,
        resolved: ResolvedTarget,
        command: String,
        projection: CompletionProjection,
    ) -> Result<OperationAdmissionResponse, ApplicationError> {
        let resolved_group_ids = public_target_group_ids(&resolved.public, &self.ids)?;
        let admission = self.operations.admit(
            context,
            principal.id(),
            None,
            scope.request_id(),
            kind,
            resolved.public,
            resolved.resolved_target_count,
            &resolved.session_ids,
            &resolved_group_ids,
            false,
            fingerprint,
        )?;
        if admission.newly_admitted {
            if let Err(error) = self.publish_operation(&admission.operation) {
                let error = error.with_operation_id(&admission.operation.operation_id);
                let _ = self.operations.fail(
                    &admission.operation.operation_id,
                    error.to_contract(scope.request_id()),
                    Vec::new(),
                );
                return Err(error);
            }
            let service = Arc::clone(self);
            let task = CommandOperationTask {
                operation_id: admission.operation.operation_id.clone(),
                request_id: admission.operation.request_id.clone(),
                target: resolved.command,
                session_ids: resolved.session_ids,
                command,
                kind,
                projection,
            };
            tokio::spawn(async move {
                service.run_command_operation(task).await;
            });
        }
        Ok(OperationAdmissionResponse {
            context: Some(scope.response_context(self.server_instance_id())),
            operation: Some(admission.operation),
        })
    }

    async fn run_command_operation(self: Arc<Self>, task: CommandOperationTask) {
        let CommandOperationTask {
            operation_id,
            request_id,
            target,
            session_ids,
            command,
            kind,
            projection,
        } = task;
        let running = match self.operations.mark_running(&operation_id) {
            Ok(operation) => operation,
            Err(_) => return,
        };
        if let Err(error) = self.publish_operation(&running) {
            warn!(
                operation_id,
                code = ?error.code(),
                "operation running event could not be published"
            );
        }

        if let Some(frame) = projection.frame_guard() {
            if let Err(error) = self.ensure_frame_current(frame).await {
                self.fail_operation(&operation_id, &request_id, error, &session_ids);
                return;
            }
        }

        match self
            .command_port
            .execute_tracked(&command, target, &operation_id, kind)
            .await
        {
            Ok(outcome) => {
                if projection.requires_concrete_target_completions() {
                    if let Some(report) = missing_target_completion_report(&session_ids, &outcome) {
                        self.fail_command_operation(
                            &operation_id,
                            &request_id,
                            CommandPortError::from_fanout(report),
                            &projection,
                            &session_ids,
                        );
                        return;
                    }
                }
                if let Some(frame) = projection.frame_guard() {
                    if let Err(error) = self.ensure_frame_current(frame).await {
                        self.fail_operation(&operation_id, &request_id, error, &session_ids);
                        return;
                    }
                }
                match self
                    .project_completion(&operation_id, &request_id, projection, &outcome)
                    .await
                {
                    Ok(result) => {
                        let outcomes = self.success_outcomes(&session_ids, &outcome);
                        match self.operations.complete(
                            &operation_id,
                            Some(result),
                            outcomes,
                            None,
                            None,
                        ) {
                            Ok(operation) => {
                                if let Err(error) = self.publish_operation(&operation) {
                                    warn!(
                                        operation_id,
                                        code = ?error.code(),
                                        "operation completion event could not be published"
                                    );
                                }
                            }
                            Err(error) => warn!(
                                operation_id,
                                code = ?error.code(),
                                "operation completion could not be retained"
                            ),
                        }
                    }
                    Err(error) => {
                        self.fail_operation(&operation_id, &request_id, error, &session_ids)
                    }
                }
            }
            Err(error) => self.fail_command_operation(
                &operation_id,
                &request_id,
                error,
                &projection,
                &session_ids,
            ),
        }
    }

    async fn run_extension_operation(self: Arc<Self>, task: ExtensionOperationTask) {
        let ExtensionOperationTask {
            operation_id,
            request_id,
            session_ids,
            registry,
            invocation,
        } = task;
        let running = match self.operations.mark_running(&operation_id) {
            Ok(operation) => operation,
            Err(_) => return,
        };
        let _ = self.publish_operation(&running);

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            registry.invoke(invocation, MAX_RETAINED_RESULT_BYTES),
        )
        .await;
        let payload = match result {
            Ok(Ok(payload)) => payload,
            Ok(Err(error)) => {
                self.fail_operation(
                    &operation_id,
                    &request_id,
                    extension_invocation_error(error),
                    &session_ids,
                );
                return;
            }
            Err(_) => {
                self.fail_operation(
                    &operation_id,
                    &request_id,
                    ApplicationError::new(
                        DdbErrorCode::DeadlineExceeded,
                        "extension action exceeded the execution deadline",
                    ),
                    &session_ids,
                );
                return;
            }
        };
        let result = OperationResult {
            value: Some(operation_result::Value::ExtensionAction(
                InvokeExtensionActionResult {
                    payload: Some(payload),
                },
            )),
        };
        let outcomes = self.successful_session_outcomes(&session_ids);
        match self
            .operations
            .complete(&operation_id, Some(result), outcomes, None, None)
        {
            Ok(operation) => {
                let _ = self.publish_operation(&operation);
            }
            Err(error) => warn!(
                operation_id,
                code = ?error.code(),
                "extension operation completion could not be retained"
            ),
        }
    }

    fn fail_command_operation(
        &self,
        operation_id: &str,
        request_id: &str,
        error: CommandPortError,
        projection: &CompletionProjection,
        admitted_session_ids: &[u64],
    ) {
        let Some(report) = error.fanout_report() else {
            self.fail_operation(
                operation_id,
                request_id,
                ApplicationError::backend("debugger command failed"),
                admitted_session_ids,
            );
            return;
        };

        let successful_sessions = report
            .completion()
            .get_responses()
            .iter()
            .map(|response| response.get_sid())
            .filter(|sid| *sid != 0)
            .collect::<HashSet<_>>();
        let mut sessions = admitted_session_ids.iter().copied().collect::<HashSet<_>>();
        sessions.extend(successful_sessions.iter().copied());
        sessions.extend(report.failures().iter().map(|failure| failure.sid()));
        let mut sessions = sessions.into_iter().collect::<Vec<_>>();
        sessions.sort_unstable();

        let mut outcomes = Vec::with_capacity(sessions.len());
        let mut target_failures = Vec::new();
        let mut has_success = false;
        let mut has_failure = false;
        let mut failures_are_timeouts = true;
        for sid in sessions {
            let failure = report
                .failures()
                .iter()
                .find(|failure| failure.sid() == sid);
            let succeeded = failure.is_none() && successful_sessions.contains(&sid);
            let Ok(target) = self.session_target(sid) else {
                continue;
            };
            if succeeded {
                has_success = true;
                outcomes.push(TargetOutcome {
                    target: Some(target),
                    succeeded: true,
                    error: None,
                });
                continue;
            }

            has_failure = true;
            let kind = failure
                .map(|failure| failure.kind())
                .unwrap_or(SessionCommandFailureKind::ExecutionFailed);
            failures_are_timeouts &= matches!(
                kind,
                SessionCommandFailureKind::AdmissionTimeout
                    | SessionCommandFailureKind::ResponseTimeout
            );
            let target_error = command_target_error(kind)
                .with_operation_id(operation_id)
                .to_contract(request_id);
            target_failures.push(TargetFailure {
                target: Some(target.clone()),
                error: Some(target_error.clone()),
            });
            outcomes.push(TargetOutcome {
                target: Some(target),
                succeeded: false,
                error: Some(target_error),
            });
        }

        if !has_failure {
            self.fail_operation(
                operation_id,
                request_id,
                ApplicationError::backend("debugger command failed"),
                admitted_session_ids,
            );
            return;
        }

        let (code, message, retryable) = if has_success {
            (
                DdbErrorCode::PartialFailure,
                "debugger command failed for one or more targets",
                false,
            )
        } else if failures_are_timeouts {
            (
                DdbErrorCode::DeadlineExceeded,
                "debugger command timed out",
                true,
            )
        } else {
            (
                DdbErrorCode::BackendFailed,
                "debugger command failed",
                false,
            )
        };
        let contract = ApplicationError::new(code, message)
            .retryable(retryable)
            .with_operation_id(operation_id)
            .with_target_failures(target_failures)
            .to_contract(request_id);
        let result = if has_success {
            match self.retained_partial_result(operation_id, request_id, projection, report) {
                Ok(result) => result,
                Err(error) => {
                    warn!(
                        operation_id,
                        code = ?error.code(),
                        "partial command result could not be projected"
                    );
                    None
                }
            }
        } else {
            None
        };
        self.retain_failed_operation(operation_id, contract, outcomes, result);
    }

    fn retained_partial_result(
        &self,
        operation_id: &str,
        request_id: &str,
        projection: &CompletionProjection,
        report: &CommandFanoutReport,
    ) -> Result<Option<OperationResult>, ApplicationError> {
        let internal_id = match projection {
            CompletionProjection::CreatedBreakpoint => {
                let outcome = CommandOutcome::silent(report.completion().clone());
                response_string(&outcome, "id")
                    .or_else(|| nested_response_string(&outcome, "bkpt", "id"))
                    .and_then(|id| id.parse::<u64>().ok())
                    .ok_or_else(|| {
                        ApplicationError::backend(
                            "partial breakpoint creation returned no logical breakpoint identity",
                        )
                    })?
            }
            CompletionProjection::DeletedBreakpoint { internal_id, .. } => *internal_id,
            _ => return Ok(basic_retained_partial_result(projection, report)),
        };
        let breakpoint = self.project_breakpoint(internal_id)?;
        self.publish_breakpoint_upsert(request_id, operation_id, &breakpoint)?;
        Ok(Some(OperationResult {
            value: Some(operation_result::Value::Breakpoint(breakpoint)),
        }))
    }

    fn fail_operation(
        &self,
        operation_id: &str,
        request_id: &str,
        error: ApplicationError,
        session_ids: &[u64],
    ) {
        let error = error.with_operation_id(operation_id);
        let contract = error.to_contract(request_id);
        let outcomes = session_ids
            .iter()
            .filter_map(|sid| {
                self.session_target(*sid).ok().map(|target| TargetOutcome {
                    target: Some(target),
                    succeeded: false,
                    error: Some(contract.clone()),
                })
            })
            .collect();
        self.retain_failed_operation(operation_id, contract, outcomes, None);
    }

    fn retain_failed_operation(
        &self,
        operation_id: &str,
        contract: ddb_api_types::v2::DdbError,
        outcomes: Vec<TargetOutcome>,
        result: Option<OperationResult>,
    ) {
        match self
            .operations
            .fail_with_result(operation_id, contract, outcomes, result)
        {
            Ok(operation) => {
                if let Err(publish_error) = self.publish_operation(&operation) {
                    warn!(
                        operation_id,
                        code = ?publish_error.code(),
                        "operation failure event could not be published"
                    );
                }
            }
            Err(store_error) => warn!(
                operation_id,
                code = ?store_error.code(),
                "operation failure could not be retained"
            ),
        }
    }

    async fn project_completion(
        &self,
        operation_id: &str,
        request_id: &str,
        projection: CompletionProjection,
        outcome: &CommandOutcome,
    ) -> Result<OperationResult, ApplicationError> {
        let value = match projection {
            CompletionProjection::NoContent => operation_result::Value::NoContent(Empty {}),
            CompletionProjection::Selection => {
                let snapshot = self.queries.snapshot().await;
                let selection = self.selection(
                    snapshot.selected_session_id,
                    snapshot.selected_thread_id,
                    &snapshot.groups,
                )?;
                self.publish_upsert(
                    request_id,
                    operation_id,
                    StateEventKind::SelectionChanged,
                    ResourceKind::Selection,
                    selection.selection_id.clone(),
                    selection.revision,
                    resource_upsert::Resource::Selection(selection.clone()),
                )?;
                operation_result::Value::Selection(selection)
            }
            CompletionProjection::Evaluation { .. } => {
                let value = response_string(outcome, "value")
                    .ok_or_else(|| ApplicationError::backend("evaluation returned no value"))?;
                let type_name = response_string(outcome, "type");
                operation_result::Value::Evaluation(EvaluationResult {
                    expression: "<redacted>".to_string(),
                    value,
                    type_name,
                    variable_id: None,
                    address: None,
                })
            }
            CompletionProjection::CreatedBreakpoint => {
                let internal_id = response_string(outcome, "id")
                    .or_else(|| nested_response_string(outcome, "bkpt", "id"))
                    .and_then(|id| id.parse::<u64>().ok())
                    .ok_or_else(|| {
                        ApplicationError::backend(
                            "breakpoint creation returned no logical breakpoint identity",
                        )
                    })?;
                let breakpoint = self.project_breakpoint(internal_id)?;
                self.publish_breakpoint_upsert(request_id, operation_id, &breakpoint)?;
                operation_result::Value::Breakpoint(breakpoint)
            }
            CompletionProjection::UpdatedBreakpoint(internal_id) => {
                let breakpoint = self.project_breakpoint(internal_id)?;
                self.publish_breakpoint_upsert(request_id, operation_id, &breakpoint)?;
                operation_result::Value::Breakpoint(breakpoint)
            }
            CompletionProjection::DeletedBreakpoint {
                internal_id,
                public_id,
                context,
            } => {
                let revision = self
                    .resources
                    .bump(ResourceIdKind::Breakpoint, internal_id)?
                    .revision;
                self.journal.publish(StateChange {
                    request_id: Some(request_id.to_string()),
                    operation_id: Some(operation_id.to_string()),
                    kind: StateEventKind::ResourceDeleted,
                    resource_kind: ResourceKind::Breakpoint,
                    resource_id: public_id.clone(),
                    resource_revision: revision,
                    payload: state_event::Payload::Deleted(ResourceDeleted {
                        resource_kind: ResourceKind::Breakpoint as i32,
                        resource_id: public_id,
                        resource_revision: revision,
                    }),
                    extension_details: Vec::new(),
                    context,
                })?;
                operation_result::Value::NoContent(Empty {})
            }
            CompletionProjection::RawCommand => {
                operation_result::Value::RawCommand(raw_command_result(outcome))
            }
            CompletionProjection::DistributedBacktrace { max_frames } => {
                operation_result::Value::DistributedBacktrace(
                    self.distributed_backtrace_result(outcome, max_frames)
                        .await?,
                )
            }
        };
        Ok(OperationResult { value: Some(value) })
    }

    fn project_breakpoint(&self, internal_id: u64) -> Result<Breakpoint, ApplicationError> {
        let snapshot = self
            .queries
            .breakpoints()
            .into_iter()
            .find(|breakpoint| breakpoint.id == internal_id)
            .ok_or_else(|| ApplicationError::not_found("breakpoint"))?;
        self.projection().breakpoint(&snapshot)
    }

    fn publish_breakpoint_upsert(
        &self,
        request_id: &str,
        operation_id: &str,
        breakpoint: &Breakpoint,
    ) -> Result<(), ApplicationError> {
        self.publish_upsert(
            request_id,
            operation_id,
            StateEventKind::ResourceUpserted,
            ResourceKind::Breakpoint,
            breakpoint.breakpoint_id.clone(),
            breakpoint.revision,
            resource_upsert::Resource::Breakpoint(breakpoint.clone()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_upsert(
        &self,
        request_id: &str,
        operation_id: &str,
        event_kind: StateEventKind,
        resource_kind: ResourceKind,
        resource_id: String,
        revision: u64,
        resource: resource_upsert::Resource,
    ) -> Result<(), ApplicationError> {
        let context = StateEventContext::from_resource(&resource);
        self.journal.publish(StateChange {
            request_id: Some(request_id.to_string()),
            operation_id: Some(operation_id.to_string()),
            kind: event_kind,
            resource_kind,
            resource_id,
            resource_revision: revision,
            payload: state_event::Payload::Upsert(ResourceUpsert {
                resource: Some(resource),
            }),
            extension_details: Vec::new(),
            context,
        })?;
        Ok(())
    }

    fn publish_operation(&self, operation: &Operation) -> Result<(), ApplicationError> {
        let resource = resource_upsert::Resource::Operation(operation.clone());
        let mut context = StateEventContext::from_resource(&resource);
        let (session_ids, group_ids) = self.operations.target_context(&operation.operation_id)?;
        for session_id in session_ids {
            context.add_session(self.ids.encode(ResourceIdKind::Session, session_id)?);
        }
        for group_id in group_ids {
            context.add_group(self.ids.encode(ResourceIdKind::Group, group_id)?);
        }
        self.journal.publish(StateChange {
            request_id: Some(operation.request_id.clone()),
            operation_id: Some(operation.operation_id.clone()),
            kind: StateEventKind::OperationChanged,
            resource_kind: ResourceKind::Operation,
            resource_id: operation.operation_id.clone(),
            resource_revision: operation.revision,
            payload: state_event::Payload::Upsert(ResourceUpsert {
                resource: Some(resource),
            }),
            extension_details: Vec::new(),
            context,
        })?;
        Ok(())
    }

    fn success_outcomes(
        &self,
        admitted_session_ids: &[u64],
        outcome: &CommandOutcome,
    ) -> Vec<TargetOutcome> {
        let mut sessions = admitted_session_ids.iter().copied().collect::<HashSet<_>>();
        if let Some(response) = outcome.response_ref() {
            sessions.extend(
                response
                    .get_responses()
                    .iter()
                    .map(|response| response.get_sid())
                    .filter(|sid| *sid != 0),
            );
        }
        let mut sessions = sessions.into_iter().collect::<Vec<_>>();
        sessions.sort_unstable();
        self.successful_session_outcomes(&sessions)
    }

    fn successful_session_outcomes(&self, session_ids: &[u64]) -> Vec<TargetOutcome> {
        session_ids
            .iter()
            .filter_map(|sid| {
                self.session_target(*sid).ok().map(|target| TargetOutcome {
                    target: Some(target),
                    succeeded: true,
                    error: None,
                })
            })
            .collect()
    }

    fn session_target(&self, sid: u64) -> Result<PublicTarget, ApplicationError> {
        Ok(PublicTarget {
            selector: Some(target::Selector::Session(SessionTarget {
                session_id: self.ids.encode(ResourceIdKind::Session, sid)?,
            })),
        })
    }

    async fn distributed_backtrace_result(
        &self,
        outcome: &CommandOutcome,
        max_frames: usize,
    ) -> Result<DistributedBacktraceResult, ApplicationError> {
        let stack = first_payload(outcome)
            .and_then(|payload| payload.as_map().get("stack"))
            .and_then(|value| match value {
                DebuggerValue::List(values) => Some(values),
                _ => None,
            })
            .ok_or_else(|| ApplicationError::backend("distributed backtrace returned no stack"))?;
        let truncated = stack.len() > max_frames;
        let mut frames = Vec::with_capacity(stack.len().min(max_frames));
        for (index, value) in stack.iter().take(max_frames).enumerate() {
            let DebuggerValue::Dict(frame) = value else {
                return Err(ApplicationError::backend(
                    "distributed backtrace returned a malformed frame",
                ));
            };
            let sid = dict_string(frame, "session")
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| ApplicationError::backend("distributed frame has no session"))?;
            let thread = dict_string(frame, "thread")
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| ApplicationError::backend("distributed frame has no thread"))?;
            let level = dict_string(frame, "level")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(index as u32);
            let session_id = self.ids.encode(ResourceIdKind::Session, sid)?;
            let thread_id = self.ids.encode(ResourceIdKind::Thread, thread)?;
            let boundary = dict_string(frame, "boundary_frame").is_some();
            if boundary {
                frames.push(DistributedFrame {
                    index: index as u32,
                    session_id,
                    thread_id,
                    frame: None,
                    boundary: Some(DistributedBoundaryKind::Call as i32),
                    boundary_label: Some("distributed call boundary".to_string()),
                });
                continue;
            }
            let thread_view = self
                .queries
                .thread_by_id(thread)
                .await
                .ok_or_else(|| ApplicationError::not_found("thread"))?;
            if thread_view.status != "stopped" {
                return Err(ApplicationError::new(
                    DdbErrorCode::Expired,
                    "distributed frame owner is no longer stopped",
                )
                .retryable(true));
            }
            let frame_key = format!("{thread}:{}:{level}", thread_view.execution_revision);
            let location = source_location(frame);
            frames.push(DistributedFrame {
                index: index as u32,
                session_id,
                thread_id: thread_id.clone(),
                frame: Some(Frame {
                    frame_id: self.ids.encode(ResourceIdKind::Frame, frame_key)?,
                    thread_id,
                    level,
                    function_name: dict_string(frame, "func").map(str::to_string),
                    location,
                    module: dict_string(frame, "from").map(str::to_string),
                    synthetic: false,
                }),
                boundary: None,
                boundary_label: None,
            });
        }
        Ok(DistributedBacktraceResult {
            frames,
            truncated,
            truncation_reason: truncated.then(|| "max_frames limit reached".to_string()),
        })
    }

    fn validate_preconditions(
        &self,
        preconditions: Option<&Preconditions>,
        resource: Option<(&str, u64)>,
    ) -> Result<(), ApplicationError> {
        let Some(preconditions) = preconditions else {
            return Ok(());
        };
        if let Some(expected) = preconditions.state_revision {
            let (_, current) = self.journal.checkpoint();
            if expected != current {
                return Err(ApplicationError::new(
                    DdbErrorCode::FailedPrecondition,
                    format!(
                        "state revision precondition does not match the current revision {current}"
                    ),
                ));
            }
        }
        if let Some(expected) = preconditions.resource_version.as_ref() {
            let Some((resource_id, revision)) = resource else {
                return Err(ApplicationError::invalid(
                    "preconditions.resource_version",
                    "is not applicable to this operation",
                ));
            };
            if expected.resource_id != resource_id || expected.revision != revision {
                return Err(ApplicationError::new(
                    DdbErrorCode::FailedPrecondition,
                    "resource version precondition does not match",
                ));
            }
        }
        Ok(())
    }

    fn decode_breakpoint_id(&self, public_id: &str) -> Result<u64, ApplicationError> {
        if public_id.trim().is_empty() {
            return Err(ApplicationError::invalid(
                "breakpoint_id",
                "must not be empty",
            ));
        }
        self.ids
            .decode(ResourceIdKind::Breakpoint, public_id)?
            .parse::<u64>()
            .map_err(|_| ApplicationError::not_found("breakpoint"))
    }

    fn finish_shutdown_operation(&self, operation_id: &str) {
        let Ok(running) = self.operations.mark_running(operation_id) else {
            return;
        };
        let _ = self.publish_operation(&running);
        let result = OperationResult {
            value: Some(operation_result::Value::NoContent(Empty {})),
        };
        let outcomes = running
            .target
            .as_ref()
            .and_then(|summary| summary.target.clone())
            .map(|target| TargetOutcome {
                target: Some(target),
                succeeded: true,
                error: None,
            })
            .into_iter()
            .collect();
        if let Ok(completed) =
            self.operations
                .complete(operation_id, Some(result), outcomes, None, None)
        {
            let _ = self.publish_operation(&completed);
        }
    }
}

fn fingerprint_without_context<M: Message + Clone>(
    request: &M,
    clear_context: impl FnOnce(&mut M),
) -> Vec<u8> {
    let mut canonical = request.clone();
    clear_context(&mut canonical);
    canonical.encode_to_vec()
}

fn extension_invocation_error(error: InvocationError) -> ApplicationError {
    match error {
        InvocationError::ExtensionNotFound => ApplicationError::not_found("extension"),
        InvocationError::ActionNotFound => ApplicationError::not_found("extension action"),
        InvocationError::InvalidPayload(reason) => ApplicationError::invalid("payload", reason),
        InvocationError::Provider(ProviderErrorKind::InvalidRequest) => {
            ApplicationError::invalid("payload", "extension provider rejected the request")
        }
        InvocationError::Provider(ProviderErrorKind::Unsupported) => ApplicationError::new(
            DdbErrorCode::Unsupported,
            "extension provider does not support the declared action",
        ),
        InvocationError::Provider(ProviderErrorKind::Unavailable) => ApplicationError::new(
            DdbErrorCode::Unavailable,
            "extension provider is unavailable",
        )
        .retryable(true),
        InvocationError::Provider(ProviderErrorKind::Failed)
        | InvocationError::InvalidResult(_) => ApplicationError::backend("extension action failed"),
    }
}

fn require_nonempty_bounded(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ApplicationError> {
    if value.trim().is_empty() {
        return Err(ApplicationError::invalid(field, "must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(ApplicationError::invalid(
            field,
            format!("must not exceed {max_bytes} UTF-8 bytes"),
        ));
    }
    Ok(())
}

fn execution_command(request: &ExecuteRequest) -> Result<String, ApplicationError> {
    let action = ExecutionAction::try_from(request.action).map_err(|_| {
        ApplicationError::invalid(
            "action",
            format!("unknown execution action {}", request.action),
        )
    })?;
    match action {
        ExecutionAction::Continue => no_execution_argument(request, "-record-time-and-continue"),
        ExecutionAction::Interrupt => no_execution_argument(request, "-exec-interrupt"),
        ExecutionAction::Next => no_execution_argument(request, "-record-time-and-next"),
        ExecutionAction::StepIn => no_execution_argument(request, "-record-time-and-step"),
        ExecutionAction::StepOut => no_execution_argument(request, "-record-time-and-finish"),
        ExecutionAction::Jump => {
            if request.signal_name.is_some() {
                return Err(ApplicationError::invalid(
                    "signal_name",
                    "is valid only for SIGNAL",
                ));
            }
            let location = request.jump_location.as_ref().ok_or_else(|| {
                ApplicationError::invalid("jump_location", "is required for JUMP")
            })?;
            Ok(format!("-exec-jump {}", quote(&jump_location(location)?)))
        }
        ExecutionAction::Signal => {
            if request.jump_location.is_some() {
                return Err(ApplicationError::invalid(
                    "jump_location",
                    "is valid only for JUMP",
                ));
            }
            let signal = request.signal_name.as_deref().ok_or_else(|| {
                ApplicationError::invalid("signal_name", "is required for SIGNAL")
            })?;
            require_nonempty_bounded("signal_name", signal, MAX_SIGNAL_BYTES)?;
            Ok(format!("-send-signal {}", quote(signal)))
        }
        ExecutionAction::ReverseContinue
        | ExecutionAction::ReverseNext
        | ExecutionAction::ReverseStepIn => Err(ApplicationError::new(
            DdbErrorCode::Unsupported,
            "reverse execution is not supported by the configured command service",
        )
        .requiring("execution.reverse")),
        ExecutionAction::Unspecified => Err(ApplicationError::invalid(
            "action",
            "UNSPECIFIED is not an execution action",
        )),
    }
}

fn validate_execution_target(action: i32, target: &CommandTarget) -> Result<(), ApplicationError> {
    let action = ExecutionAction::try_from(action).map_err(|_| {
        ApplicationError::invalid("action", format!("unknown execution action {action}"))
    })?;
    match action {
        ExecutionAction::Next | ExecutionAction::StepIn | ExecutionAction::StepOut
            if !matches!(target, CommandTarget::Thread(_)) =>
        {
            Err(ApplicationError::invalid(
                "target",
                "step, next, and step-out actions require a thread target",
            ))
        }
        ExecutionAction::Jump | ExecutionAction::Signal
            if !matches!(target, CommandTarget::Thread(_) | CommandTarget::Session(_)) =>
        {
            Err(ApplicationError::invalid(
                "target",
                "jump and signal actions require a thread or session target",
            ))
        }
        ExecutionAction::Continue
        | ExecutionAction::Interrupt
        | ExecutionAction::Next
        | ExecutionAction::StepIn
        | ExecutionAction::StepOut
        | ExecutionAction::Jump
        | ExecutionAction::Signal => Ok(()),
        ExecutionAction::ReverseContinue
        | ExecutionAction::ReverseNext
        | ExecutionAction::ReverseStepIn
        | ExecutionAction::Unspecified => Ok(()),
    }
}

fn validate_raw_command_target(
    command: &ParsedInputCmd,
    target: &CommandTarget,
) -> Result<(), ApplicationError> {
    fn is_breakpoint_scope(target: &CommandTarget) -> bool {
        match target {
            CommandTarget::Session(_) | CommandTarget::Group(_) => true,
            CommandTarget::Multiple(targets) => {
                !targets.is_empty() && targets.iter().all(is_breakpoint_scope)
            }
            _ => false,
        }
    }

    if command.prefix == "-break-insert" && !is_breakpoint_scope(target) {
        return Err(ApplicationError::invalid(
            "target",
            "DDB/MI break-insert requires a session, group, or multiple target containing only sessions and groups",
        ));
    }
    Ok(())
}

fn no_execution_argument(
    request: &ExecuteRequest,
    command: &'static str,
) -> Result<String, ApplicationError> {
    if request.jump_location.is_some() || request.signal_name.is_some() {
        Err(ApplicationError::invalid(
            "jump_location",
            "jump_location and signal_name are action-specific",
        ))
    } else {
        Ok(command.to_string())
    }
}

fn public_target_group_ids(
    target: &PublicTarget,
    ids: &super::OpaqueIdRegistry,
) -> Result<Vec<u64>, ApplicationError> {
    fn collect(
        target: &PublicTarget,
        ids: &super::OpaqueIdRegistry,
        groups: &mut HashSet<u64>,
    ) -> Result<(), ApplicationError> {
        match target.selector.as_ref() {
            Some(target::Selector::Group(group)) => {
                let internal = ids
                    .decode(ResourceIdKind::Group, &group.group_id)?
                    .parse::<u64>()
                    .map_err(|_| ApplicationError::not_found("group"))?;
                groups.insert(internal);
            }
            Some(target::Selector::Multiple(multiple)) => {
                for target in &multiple.targets {
                    collect(target, ids, groups)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    let mut groups = HashSet::new();
    collect(target, ids, &mut groups)?;
    let mut groups = groups.into_iter().collect::<Vec<_>>();
    groups.sort_unstable();
    Ok(groups)
}

fn jump_location(location: &SourceLocation) -> Result<String, ApplicationError> {
    if location.column != 0 {
        return Err(ApplicationError::new(
            DdbErrorCode::Unsupported,
            "column-specific jump locations are not supported",
        )
        .requiring("execution.jump.column"));
    }
    if let Some(address) = location.address.as_deref() {
        require_nonempty_bounded("jump_location.address", address, MAX_COMMAND_BYTES)?;
        return Ok(address.to_string());
    }
    if let Some(path) = location.path.as_deref() {
        require_nonempty_bounded("jump_location.path", path, MAX_COMMAND_BYTES)?;
        if location.line == 0 {
            return Err(ApplicationError::invalid(
                "jump_location.line",
                "must be greater than zero with a path",
            ));
        }
        return Ok(format!("{path}:{}", location.line));
    }
    if let Some(function) = location.function_name.as_deref() {
        require_nonempty_bounded("jump_location.function_name", function, MAX_COMMAND_BYTES)?;
        return Ok(function.to_string());
    }
    Err(ApplicationError::invalid(
        "jump_location",
        "must contain an address, path and line, or function name",
    ))
}

fn breakpoint_definition(
    spec: &BreakpointSpec,
) -> Result<(BkptLoc, BreakpointProperties), ApplicationError> {
    if spec.ignore_count.is_some() {
        return Err(ApplicationError::new(
            DdbErrorCode::Unsupported,
            "breakpoint ignore counts are not currently supported",
        )
        .requiring("breakpoints.ignore_count"));
    }
    if let Some(condition) = spec.condition.as_deref() {
        require_nonempty_bounded("breakpoint.condition", condition, MAX_COMMAND_BYTES)?;
    }
    let source = match spec.location.as_ref() {
        Some(breakpoint_spec::Location::Source(source)) => source,
        Some(breakpoint_spec::Location::Function(_)) => {
            return Err(ApplicationError::new(
                DdbErrorCode::Unsupported,
                "function breakpoints are not currently supported",
            )
            .requiring("breakpoints.function"))
        }
        Some(breakpoint_spec::Location::Address(_)) => {
            return Err(ApplicationError::new(
                DdbErrorCode::Unsupported,
                "address breakpoints are not currently supported",
            )
            .requiring("breakpoints.address"))
        }
        None => {
            return Err(ApplicationError::invalid(
                "breakpoint.location",
                "is required",
            ))
        }
    };
    require_nonempty_bounded(
        "breakpoint.location.source.source",
        &source.source,
        MAX_COMMAND_BYTES,
    )?;
    if source.line == 0 {
        return Err(ApplicationError::invalid(
            "breakpoint.location.source.line",
            "must be greater than zero",
        ));
    }
    if source.column != 0 {
        return Err(ApplicationError::new(
            DdbErrorCode::Unsupported,
            "column-specific source breakpoints are not supported",
        )
        .requiring("breakpoints.source.column"));
    }
    Ok((
        BkptLoc::new(&source.source, u64::from(source.line)),
        BreakpointProperties {
            enabled: spec.enabled.unwrap_or(true),
            condition: spec.condition.clone(),
            temporary: spec.temporary,
            hardware: spec.hardware,
        },
    ))
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

fn first_payload(outcome: &CommandOutcome) -> Option<&Dict> {
    outcome
        .response_ref()?
        .get_responses()
        .iter()
        .find_map(|response| response.get_payload())
}

fn response_string(outcome: &CommandOutcome, key: &str) -> Option<String> {
    first_payload(outcome)
        .and_then(|payload| dict_string(payload, key))
        .map(str::to_string)
}

fn nested_response_string(outcome: &CommandOutcome, outer: &str, inner: &str) -> Option<String> {
    let payload = first_payload(outcome)?;
    let DebuggerValue::Dict(nested) = payload.as_map().get(outer)? else {
        return None;
    };
    dict_string(nested, inner).map(str::to_string)
}

fn dict_string<'a>(dict: &'a Dict, key: &str) -> Option<&'a str> {
    match dict.as_map().get(key)? {
        DebuggerValue::String(value) => Some(value),
        _ => None,
    }
}

fn source_location(frame: &Dict) -> Option<SourceLocation> {
    let path = dict_string(frame, "fullname")
        .or_else(|| dict_string(frame, "file"))
        .map(str::to_string);
    let line = dict_string(frame, "line")
        .and_then(|line| line.parse::<u32>().ok())
        .unwrap_or(0);
    let address = dict_string(frame, "addr").map(str::to_string);
    (path.is_some() || address.is_some()).then(|| SourceLocation {
        source_reference: None,
        path,
        line,
        column: 0,
        address,
        function_name: dict_string(frame, "func").map(str::to_string),
    })
}

fn raw_command_result(outcome: &CommandOutcome) -> RawCommandResult {
    let text = outcome.response_ref().and_then(|response| {
        let joined = response
            .get_responses()
            .iter()
            .map(|response| response.get_message().as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (!joined.is_empty()).then_some(joined)
    });
    let mut nodes = 0;
    let value = first_payload(outcome).and_then(|payload| dynamic_dict(payload, 0, &mut nodes));
    let mut result = RawCommandResult {
        value,
        text,
        truncated: false,
    };
    if result.encoded_len() > MAX_RETAINED_RESULT_BYTES {
        result = RawCommandResult {
            value: None,
            text: None,
            truncated: true,
        };
    }
    result
}

fn basic_retained_partial_result(
    projection: &CompletionProjection,
    report: &CommandFanoutReport,
) -> Option<OperationResult> {
    let outcome = CommandOutcome::silent(report.completion().clone());
    let value = match projection {
        CompletionProjection::NoContent => operation_result::Value::NoContent(Empty {}),
        CompletionProjection::RawCommand => {
            operation_result::Value::RawCommand(raw_command_result(&outcome))
        }
        CompletionProjection::Selection
        | CompletionProjection::Evaluation { .. }
        | CompletionProjection::CreatedBreakpoint
        | CompletionProjection::UpdatedBreakpoint(_)
        | CompletionProjection::DeletedBreakpoint { .. }
        | CompletionProjection::DistributedBacktrace { .. } => return None,
    };
    Some(OperationResult { value: Some(value) })
}

fn missing_target_completion_report(
    admitted_session_ids: &[u64],
    outcome: &CommandOutcome,
) -> Option<CommandFanoutReport> {
    let completion = outcome.response_ref()?;
    let completed_sessions = completion
        .get_responses()
        .iter()
        .map(|response| response.get_sid())
        .filter(|sid| *sid != 0)
        .collect::<HashSet<_>>();
    if completed_sessions.is_empty() {
        return None;
    }
    let failures = admitted_session_ids
        .iter()
        .copied()
        .filter(|sid| !completed_sessions.contains(sid))
        .map(|sid| SessionCommandFailure::new(sid, SessionCommandFailureKind::ExecutionFailed))
        .collect::<Vec<_>>();
    (!failures.is_empty()).then(|| {
        CommandFanoutReport::new(
            completion.get_external_token(),
            completion.get_responses().clone(),
            failures,
        )
    })
}

fn command_target_error(kind: SessionCommandFailureKind) -> ApplicationError {
    match kind {
        SessionCommandFailureKind::AdmissionTimeout
        | SessionCommandFailureKind::ResponseTimeout => ApplicationError::new(
            DdbErrorCode::DeadlineExceeded,
            "debugger command timed out for target",
        ),
        SessionCommandFailureKind::AdmissionRejected => ApplicationError::new(
            DdbErrorCode::Unavailable,
            "debugger session was unavailable for command admission",
        ),
        SessionCommandFailureKind::DebuggerRejected => ApplicationError::new(
            DdbErrorCode::BackendFailed,
            "debugger rejected command for target",
        ),
        SessionCommandFailureKind::ResponseFailed | SessionCommandFailureKind::ExecutionFailed => {
            ApplicationError::new(
                DdbErrorCode::BackendFailed,
                "debugger command failed for target",
            )
        }
    }
}

fn dynamic_dict(dict: &Dict, depth: usize, nodes: &mut usize) -> Option<DynamicValue> {
    if depth > 32 || *nodes >= MAX_DYNAMIC_NODES {
        return None;
    }
    *nodes += 1;
    let mut fields = std::collections::HashMap::new();
    for (key, value) in dict.as_map() {
        fields.insert(key.clone(), dynamic_value(value, depth + 1, nodes)?);
    }
    Some(DynamicValue {
        kind: Some(dynamic_value::Kind::ObjectValue(DynamicObject { fields })),
    })
}

fn dynamic_value(value: &DebuggerValue, depth: usize, nodes: &mut usize) -> Option<DynamicValue> {
    if depth > 32 || *nodes >= MAX_DYNAMIC_NODES {
        return None;
    }
    *nodes += 1;
    let kind = match value {
        DebuggerValue::String(value) => dynamic_value::Kind::StringValue(value.clone()),
        DebuggerValue::List(values) => dynamic_value::Kind::ListValue(DynamicList {
            values: values
                .iter()
                .map(|value| dynamic_value(value, depth + 1, nodes))
                .collect::<Option<Vec<_>>>()?,
        }),
        DebuggerValue::Dict(dict) => {
            let mut fields = std::collections::HashMap::new();
            for (key, value) in dict.as_map() {
                fields.insert(key.clone(), dynamic_value(value, depth + 1, nodes)?);
            }
            dynamic_value::Kind::ObjectValue(DynamicObject { fields })
        }
    };
    Some(DynamicValue { kind: Some(kind) })
}

#[cfg(test)]
mod tests {
    use crate::cmd_flow::{FinishedCmd, ParsedSessionResponse};

    use super::*;

    #[test]
    fn missing_concrete_completion_becomes_a_target_failure() {
        let outcome = CommandOutcome::silent(FinishedCmd::new(
            Some(51),
            7,
            vec![ParsedSessionResponse::new(7, "done".to_string(), None)],
        ));

        let report = missing_target_completion_report(&[7, 8], &outcome)
            .expect("the missing admitted session must not be treated as success");
        assert_eq!(report.completion().get_external_token(), Some(51));
        assert_eq!(report.completion().get_responses().len(), 1);
        assert_eq!(
            report.failures(),
            &[SessionCommandFailure::new(
                8,
                SessionCommandFailureKind::ExecutionFailed
            )]
        );
    }

    #[test]
    fn exact_concrete_completions_need_no_synthetic_failure() {
        let outcome = CommandOutcome::silent(FinishedCmd::new(
            None,
            7,
            vec![
                ParsedSessionResponse::new(7, "done".to_string(), None),
                ParsedSessionResponse::new(8, "done".to_string(), None),
            ],
        ));

        assert!(missing_target_completion_report(&[7, 8], &outcome).is_none());
    }

    #[test]
    fn raw_breakpoint_insert_requires_a_breakpoint_scope() {
        let command: ParsedInputCmd = "-break-insert /tmp/main.rs:7".try_into().unwrap();

        assert!(validate_raw_command_target(&command, &CommandTarget::Session(3)).is_ok());
        assert!(validate_raw_command_target(
            &command,
            &CommandTarget::Multiple(vec![
                CommandTarget::Session(3),
                CommandTarget::Group(crate::state::GroupId::new(4)),
            ]),
        )
        .is_ok());
        assert!(validate_raw_command_target(
            &command,
            &CommandTarget::Thread(crate::state::GlobalThreadId::new(9)),
        )
        .is_err());
    }
}
#[test]
fn typed_execution_defaults_to_ddb_history_recording() {
    for (action, expected) in [
        (ExecutionAction::Continue, "-record-time-and-continue"),
        (ExecutionAction::Next, "-record-time-and-next"),
        (ExecutionAction::StepIn, "-record-time-and-step"),
        (ExecutionAction::StepOut, "-record-time-and-finish"),
    ] {
        let request = ExecuteRequest {
            action: action as i32,
            ..Default::default()
        };
        assert_eq!(execution_command(&request).unwrap(), expected);
    }
    let interrupt = ExecuteRequest {
        action: ExecutionAction::Interrupt as i32,
        ..Default::default()
    };
    assert_eq!(execution_command(&interrupt).unwrap(), "-exec-interrupt");
}
