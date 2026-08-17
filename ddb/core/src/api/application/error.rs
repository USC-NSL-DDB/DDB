use ddb_api_types::v2::{
    Cursor, DdbError, DdbErrorCode, ErrorDetail, FieldViolation, TargetFailure,
};

/// Semantic failure shared by every v2 adapter.
///
/// The application layer deliberately does not carry an HTTP status or gRPC
/// status. Adapters map code and serialize to_contract without parsing
/// human-readable messages.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct ApplicationError {
    code: DdbErrorCode,
    message: String,
    retryable: bool,
    operation_id: Option<String>,
    metadata: Option<Box<ApplicationErrorMetadata>>,
}

#[derive(Clone, Debug, Default)]
struct ApplicationErrorMetadata {
    field_violations: Vec<FieldViolation>,
    target_failures: Vec<TargetFailure>,
    earliest_cursor: Option<Cursor>,
    current_cursor: Option<Cursor>,
    required_capability: Option<String>,
    details: Vec<ErrorDetail>,
}

impl ApplicationError {
    pub(crate) fn new(code: DdbErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            operation_id: None,
            metadata: None,
        }
    }

    pub(crate) fn invalid(field: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(DdbErrorCode::InvalidArgument, "request validation failed")
            .with_field_violation(field, description)
    }

    pub(crate) fn not_found(resource: &'static str) -> Self {
        Self::new(DdbErrorCode::NotFound, format!("{resource} was not found"))
    }

    pub(crate) fn resource_exhausted(message: impl Into<String>) -> Self {
        Self::new(DdbErrorCode::ResourceExhausted, message)
    }

    pub(crate) fn deadline_exceeded() -> Self {
        Self::new(
            DdbErrorCode::DeadlineExceeded,
            "request deadline has elapsed",
        )
    }

    pub(crate) fn backend(message: impl Into<String>) -> Self {
        Self::new(DdbErrorCode::BackendFailed, message)
    }

    pub(crate) fn code(&self) -> DdbErrorCode {
        self.code
    }

    pub(crate) fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub(crate) fn with_operation_id(mut self, operation_id: impl Into<String>) -> Self {
        self.operation_id = Some(operation_id.into());
        self
    }

    pub(crate) fn with_field_violation(
        mut self,
        field: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.metadata_mut().field_violations.push(FieldViolation {
            field: field.into(),
            description: description.into(),
        });
        self
    }

    pub(crate) fn with_replay_bounds(mut self, earliest: Cursor, current: Cursor) -> Self {
        let metadata = self.metadata_mut();
        metadata.earliest_cursor = Some(earliest);
        metadata.current_cursor = Some(current);
        self
    }

    pub(crate) fn requiring(mut self, capability: impl Into<String>) -> Self {
        self.metadata_mut().required_capability = Some(capability.into());
        self
    }

    pub(crate) fn with_target_failures(mut self, target_failures: Vec<TargetFailure>) -> Self {
        self.metadata_mut().target_failures = target_failures;
        self
    }

    fn metadata_mut(&mut self) -> &mut ApplicationErrorMetadata {
        self.metadata
            .get_or_insert_with(|| Box::new(ApplicationErrorMetadata::default()))
    }

    pub(crate) fn to_contract(&self, request_id: impl Into<String>) -> DdbError {
        let metadata = self.metadata.as_deref().cloned().unwrap_or_default();
        DdbError {
            code: self.code as i32,
            message: self.message.clone(),
            request_id: request_id.into(),
            operation_id: self.operation_id.clone(),
            retryable: self.retryable,
            retry_after: None,
            field_violations: metadata.field_violations,
            target_failures: metadata.target_failures,
            earliest_cursor: metadata.earliest_cursor,
            current_cursor: metadata.current_cursor,
            required_capability: metadata.required_capability,
            details: metadata.details,
        }
    }
}
