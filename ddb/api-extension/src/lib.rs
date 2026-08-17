//! Public, transport-independent extension boundary for DDB.
//!
//! Providers own namespaced schemas and payload semantics. The registry owns
//! the safety boundary: registration is validated once, dynamic state failure
//! is isolated per provider, and action requests/results are bounded and
//! checked before they cross the provider boundary.

use std::{collections::BTreeMap, fmt, sync::Arc};

use async_trait::async_trait;
use ddb_api_types::v2::{
    extension_payload, ExtensionActionDescriptor, ExtensionDescriptor, ExtensionPayload,
    ExtensionPresentationKind, PermissionScope, Target,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Maximum extension providers in one registry.
pub const MAX_EXTENSIONS: usize = 64;
/// Maximum schema documents owned by one extension.
pub const MAX_SCHEMAS_PER_EXTENSION: usize = 64;
/// Maximum bytes in one registered schema document.
pub const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
/// Maximum actions declared by one extension.
pub const MAX_ACTIONS: usize = 32;
/// Maximum event types declared by one extension.
pub const MAX_EVENTS: usize = 64;
/// Maximum generic presentations declared by one extension.
pub const MAX_PRESENTATIONS: usize = 32;
/// Maximum columns in one table presentation.
pub const MAX_COLUMNS: usize = 32;
/// Maximum state payload envelopes returned by one provider.
pub const MAX_STATE_PAYLOADS: usize = 16;
/// Maximum nesting depth of a JSON extension payload.
pub const MAX_JSON_DEPTH: usize = 64;
/// Maximum scalar/container nodes in a JSON extension payload.
pub const MAX_JSON_NODES: usize = 10_000;
/// Maximum bytes in an extension, action, presentation, or column identifier.
pub const MAX_ID_BYTES: usize = 128;
/// Maximum bytes in one human-readable descriptor field.
pub const MAX_TEXT_BYTES: usize = 4_096;
/// Maximum bytes in one schema URI.
pub const MAX_URI_BYTES: usize = 2_048;

/// One schema document owned by an extension provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionSchema {
    pub uri: String,
    pub media_type: String,
    pub content: Vec<u8>,
}

/// A detached extension state projection. DDB assigns its public resource ID
/// and revision after registry validation.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionStateSnapshot {
    pub extension_id: String,
    pub payloads: Vec<ExtensionPayload>,
}

/// A validated action invocation passed to the owning provider.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionInvocation {
    pub extension_id: String,
    pub action_id: String,
    pub payload: ExtensionPayload,
    pub target: Target,
}

/// Provider failures are intentionally coarser than DDB's core error model.
/// `message` is returned to the registry but must not be logged or exposed by
/// a host unless the provider explicitly documents it as safe.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn unsupported() -> Self {
        Self::new(
            ProviderErrorKind::Unsupported,
            "the extension does not implement this action",
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    InvalidRequest,
    Unsupported,
    Unavailable,
    Failed,
}

/// Implemented by framework integrations or independent extension crates.
/// Providers never receive bearer credentials or transport objects.
#[async_trait]
pub trait ExtensionProvider: Send + Sync + fmt::Debug {
    fn descriptor(&self) -> ExtensionDescriptor;

    fn schemas(&self) -> Vec<ExtensionSchema>;

    fn state(&self) -> Result<Vec<ExtensionPayload>, ProviderError> {
        Ok(Vec::new())
    }

    async fn invoke(
        &self,
        _invocation: ExtensionInvocation,
    ) -> Result<ExtensionPayload, ProviderError> {
        Err(ProviderError::unsupported())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("extension registry exceeds the limit of {MAX_EXTENSIONS} providers")]
    TooManyExtensions,
    #[error("extension ID {0:?} is registered more than once")]
    DuplicateExtension(String),
    #[error("schema URI {0:?} is registered more than once")]
    DuplicateSchema(String),
    #[error("extension {extension_id:?} has an invalid {field}: {reason}")]
    InvalidDescriptor {
        extension_id: String,
        field: String,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionStateFailureKind {
    ProviderFailed,
    InvalidPayload,
}

/// Sanitized state failure suitable for metrics/log metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionStateFailure {
    pub extension_id: String,
    pub kind: ExtensionStateFailureKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtensionStateCollection {
    pub states: Vec<ExtensionStateSnapshot>,
    pub failures: Vec<ExtensionStateFailure>,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum InvocationError {
    #[error("extension is not registered")]
    ExtensionNotFound,
    #[error("extension action is not registered")]
    ActionNotFound,
    #[error("extension action payload is invalid: {0}")]
    InvalidPayload(String),
    #[error("extension provider rejected the action")]
    Provider(ProviderErrorKind),
    #[error("extension provider returned an invalid result: {0}")]
    InvalidResult(String),
}

struct RegisteredExtension {
    descriptor: ExtensionDescriptor,
    provider: Arc<dyn ExtensionProvider>,
    schemas: BTreeMap<String, ExtensionSchema>,
}

/// Immutable descriptor/schema registry with bounded dynamic dispatch.
pub struct ExtensionRegistry {
    entries: BTreeMap<String, RegisteredExtension>,
    max_payload_bytes: usize,
}

impl fmt::Debug for ExtensionRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExtensionRegistry")
            .field("extension_ids", &self.entries.keys().collect::<Vec<_>>())
            .field("max_payload_bytes", &self.max_payload_bytes)
            .finish()
    }
}

impl ExtensionRegistry {
    pub fn new(
        providers: Vec<Arc<dyn ExtensionProvider>>,
        max_payload_bytes: usize,
    ) -> Result<Self, RegistryError> {
        if providers.len() > MAX_EXTENSIONS {
            return Err(RegistryError::TooManyExtensions);
        }
        if max_payload_bytes == 0 {
            return Err(invalid(
                "<registry>",
                "max_payload_bytes",
                "must be non-zero",
            ));
        }

        let mut entries = BTreeMap::new();
        let mut schema_owners = BTreeMap::<String, String>::new();
        for provider in providers {
            let descriptor = provider.descriptor();
            validate_descriptor(&descriptor)?;
            let extension_id = descriptor.extension_id.clone();
            if entries.contains_key(&extension_id) {
                return Err(RegistryError::DuplicateExtension(extension_id));
            }

            let schemas = provider.schemas();
            if schemas.is_empty() || schemas.len() > MAX_SCHEMAS_PER_EXTENSION {
                return Err(invalid(
                    &extension_id,
                    "schemas",
                    format!("must contain between 1 and {MAX_SCHEMAS_PER_EXTENSION} entries"),
                ));
            }
            let mut registered_schemas = BTreeMap::new();
            for schema in schemas {
                validate_schema(&extension_id, &schema)?;
                if let Some(owner) = schema_owners.insert(schema.uri.clone(), extension_id.clone())
                {
                    return Err(RegistryError::DuplicateSchema(format!(
                        "{} (owners {owner:?} and {extension_id:?})",
                        schema.uri
                    )));
                }
                if registered_schemas
                    .insert(schema.uri.clone(), schema)
                    .is_some()
                {
                    return Err(RegistryError::DuplicateSchema(extension_id.clone()));
                }
            }
            validate_declared_schemas(&descriptor, &registered_schemas)?;
            entries.insert(
                extension_id,
                RegisteredExtension {
                    descriptor,
                    provider,
                    schemas: registered_schemas,
                },
            );
        }

        Ok(Self {
            entries,
            max_payload_bytes,
        })
    }

    pub fn descriptors(&self) -> Vec<ExtensionDescriptor> {
        self.entries
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect()
    }

    pub fn descriptor(&self, extension_id: &str) -> Option<&ExtensionDescriptor> {
        self.entries
            .get(extension_id)
            .map(|entry| &entry.descriptor)
    }

    pub fn action(
        &self,
        extension_id: &str,
        action_id: &str,
    ) -> Option<&ExtensionActionDescriptor> {
        self.descriptor(extension_id)?
            .actions
            .iter()
            .find(|action| action.id == action_id)
    }

    pub fn has_actions(&self) -> bool {
        self.entries
            .values()
            .any(|entry| !entry.descriptor.actions.is_empty())
    }

    /// Collect each provider independently. A broken extension is omitted and
    /// reported without preventing core snapshot or event delivery.
    pub fn collect_states(&self) -> ExtensionStateCollection {
        let mut collection = ExtensionStateCollection::default();
        for (extension_id, entry) in &self.entries {
            let payloads = match entry.provider.state() {
                Ok(payloads) => payloads,
                Err(_) => {
                    collection.failures.push(ExtensionStateFailure {
                        extension_id: extension_id.clone(),
                        kind: ExtensionStateFailureKind::ProviderFailed,
                    });
                    continue;
                }
            };
            if payloads.len() > MAX_STATE_PAYLOADS
                || validate_payload_set(
                    extension_id,
                    &entry.descriptor.version,
                    &entry.descriptor.schema_uri,
                    &payloads,
                    self.max_payload_bytes,
                )
                .is_err()
            {
                collection.failures.push(ExtensionStateFailure {
                    extension_id: extension_id.clone(),
                    kind: ExtensionStateFailureKind::InvalidPayload,
                });
                continue;
            }
            collection.states.push(ExtensionStateSnapshot {
                extension_id: extension_id.clone(),
                payloads,
            });
        }
        collection
    }

    pub fn validate_invocation(
        &self,
        invocation: &ExtensionInvocation,
    ) -> Result<(), InvocationError> {
        let entry = self
            .entries
            .get(&invocation.extension_id)
            .ok_or(InvocationError::ExtensionNotFound)?;
        let action = entry
            .descriptor
            .actions
            .iter()
            .find(|action| action.id == invocation.action_id)
            .ok_or(InvocationError::ActionNotFound)?;
        validate_payload(
            &invocation.extension_id,
            &entry.descriptor.version,
            &action.request_schema_uri,
            &invocation.payload,
            self.max_payload_bytes,
        )
        .map_err(InvocationError::InvalidPayload)
    }

    pub async fn invoke(
        &self,
        invocation: ExtensionInvocation,
        max_result_bytes: usize,
    ) -> Result<ExtensionPayload, InvocationError> {
        self.validate_invocation(&invocation)?;
        let entry = self
            .entries
            .get(&invocation.extension_id)
            .expect("validated extension must remain registered");
        let action = entry
            .descriptor
            .actions
            .iter()
            .find(|action| action.id == invocation.action_id)
            .expect("validated action must remain registered");
        let result = entry
            .provider
            .invoke(invocation)
            .await
            .map_err(|error| InvocationError::Provider(error.kind))?;
        validate_payload(
            &entry.descriptor.extension_id,
            &entry.descriptor.version,
            &action.response_schema_uri,
            &result,
            max_result_bytes.min(self.max_payload_bytes),
        )
        .map_err(InvocationError::InvalidResult)?;
        Ok(result)
    }

    pub fn schema(&self, extension_id: &str, uri: &str) -> Option<&ExtensionSchema> {
        self.entries.get(extension_id)?.schemas.get(uri)
    }
}

fn validate_descriptor(descriptor: &ExtensionDescriptor) -> Result<(), RegistryError> {
    let id = descriptor.extension_id.as_str();
    if !valid_namespaced_id(id) {
        return Err(invalid(
            id,
            "extension_id",
            "must be a project-qualified ASCII identifier containing a dot",
        ));
    }
    bounded(id, "owner", &descriptor.owner, MAX_TEXT_BYTES)?;
    bounded(id, "version", &descriptor.version, MAX_ID_BYTES)?;
    bounded(id, "title", &descriptor.title, MAX_TEXT_BYTES)?;
    bounded(id, "description", &descriptor.description, MAX_TEXT_BYTES)?;
    validate_uri(id, "schema_uri", &descriptor.schema_uri)?;
    if descriptor.schema_hash.len() != 64
        || !descriptor
            .schema_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid(
            id,
            "schema_hash",
            "must be a lowercase SHA-256 hex digest",
        ));
    }
    if descriptor.required_scopes != [PermissionScope::Read as i32] {
        return Err(invalid(
            id,
            "required_scopes",
            "extension state currently requires exactly READ; actions declare CONTROL or ADMIN independently",
        ));
    }
    if descriptor.actions.len() > MAX_ACTIONS {
        return Err(invalid(id, "actions", format!("exceeds {MAX_ACTIONS}")));
    }
    if descriptor.events.len() > MAX_EVENTS {
        return Err(invalid(id, "events", format!("exceeds {MAX_EVENTS}")));
    }
    if descriptor.presentations.len() > MAX_PRESENTATIONS {
        return Err(invalid(
            id,
            "presentations",
            format!("exceeds {MAX_PRESENTATIONS}"),
        ));
    }

    let mut action_ids = BTreeMap::new();
    for action in &descriptor.actions {
        validate_local_id(id, "actions[].id", &action.id)?;
        if action_ids.insert(action.id.as_str(), ()).is_some() {
            return Err(invalid(id, "actions[].id", "contains a duplicate"));
        }
        bounded(id, "actions[].title", &action.title, MAX_TEXT_BYTES)?;
        if let Some(description) = action.description.as_deref() {
            bounded(id, "actions[].description", description, MAX_TEXT_BYTES)?;
        }
        let required_scope = PermissionScope::try_from(action.required_scope)
            .map_err(|_| invalid(id, "actions[].required_scope", "contains an unknown value"))?;
        if !matches!(
            required_scope,
            PermissionScope::Control | PermissionScope::Admin
        ) {
            return Err(invalid(
                id,
                "actions[].required_scope",
                "must be CONTROL or ADMIN",
            ));
        }
        validate_uri(
            id,
            "actions[].request_schema_uri",
            &action.request_schema_uri,
        )?;
        validate_uri(
            id,
            "actions[].response_schema_uri",
            &action.response_schema_uri,
        )?;
    }

    let mut event_types = BTreeMap::new();
    let event_prefix = format!("{id}.");
    for event in &descriptor.events {
        if !event.event_type.starts_with(&event_prefix) || !valid_namespaced_id(&event.event_type) {
            return Err(invalid(
                id,
                "events[].event_type",
                "must be namespaced beneath the extension ID",
            ));
        }
        if event_types.insert(event.event_type.as_str(), ()).is_some() {
            return Err(invalid(id, "events[].event_type", "contains a duplicate"));
        }
        validate_uri(id, "events[].schema_uri", &event.schema_uri)?;
    }

    let mut presentation_ids = BTreeMap::new();
    for presentation in &descriptor.presentations {
        validate_local_id(id, "presentations[].id", &presentation.id)?;
        if presentation_ids
            .insert(presentation.id.as_str(), ())
            .is_some()
        {
            return Err(invalid(id, "presentations[].id", "contains a duplicate"));
        }
        bounded(
            id,
            "presentations[].title",
            &presentation.title,
            MAX_TEXT_BYTES,
        )?;
        if let Some(description) = presentation.description.as_deref() {
            bounded(
                id,
                "presentations[].description",
                description,
                MAX_TEXT_BYTES,
            )?;
        }
        let kind = ExtensionPresentationKind::try_from(presentation.kind)
            .map_err(|_| invalid(id, "presentations[].kind", "contains an unknown value"))?;
        if kind == ExtensionPresentationKind::Unspecified {
            return Err(invalid(
                id,
                "presentations[].kind",
                "must not be UNSPECIFIED",
            ));
        }
        if presentation.columns.len() > MAX_COLUMNS {
            return Err(invalid(
                id,
                "presentations[].columns",
                format!("exceeds {MAX_COLUMNS}"),
            ));
        }
        let mut column_ids = BTreeMap::new();
        for column in &presentation.columns {
            validate_local_id(id, "presentations[].columns[].id", &column.id)?;
            if column_ids.insert(column.id.as_str(), ()).is_some() {
                return Err(invalid(
                    id,
                    "presentations[].columns[].id",
                    "contains a duplicate",
                ));
            }
            bounded(
                id,
                "presentations[].columns[].title",
                &column.title,
                MAX_TEXT_BYTES,
            )?;
        }
        match kind {
            ExtensionPresentationKind::Table if presentation.columns.is_empty() => {
                return Err(invalid(
                    id,
                    "presentations[].columns",
                    "table presentations require at least one column",
                ));
            }
            ExtensionPresentationKind::Table => {
                if presentation.action_id.is_some() {
                    return Err(invalid(
                        id,
                        "presentations[].action_id",
                        "is valid only for ACTION presentations",
                    ));
                }
            }
            ExtensionPresentationKind::Action => {
                if !presentation.columns.is_empty() {
                    return Err(invalid(
                        id,
                        "presentations[].columns",
                        "action presentations do not use columns",
                    ));
                }
                let action_id = presentation.action_id.as_deref().ok_or_else(|| {
                    invalid(
                        id,
                        "presentations[].action_id",
                        "is required for ACTION presentations",
                    )
                })?;
                if !action_ids.contains_key(action_id) {
                    return Err(invalid(
                        id,
                        "presentations[].action_id",
                        "does not reference a declared action",
                    ));
                }
            }
            _ => {
                if !presentation.columns.is_empty() || presentation.action_id.is_some() {
                    return Err(invalid(
                        id,
                        "presentations",
                        "only TABLE uses columns and only ACTION uses action_id",
                    ));
                }
            }
        }
    }

    if descriptor.minimum_api_version.as_deref() != Some("v2") {
        return Err(invalid(id, "minimum_api_version", "must currently be v2"));
    }
    if descriptor
        .maximum_api_version
        .as_deref()
        .is_some_and(|version| version != "v2")
    {
        return Err(invalid(id, "maximum_api_version", "must be omitted or v2"));
    }
    Ok(())
}

fn validate_schema(extension_id: &str, schema: &ExtensionSchema) -> Result<(), RegistryError> {
    validate_uri(extension_id, "schemas[].uri", &schema.uri)?;
    if schema.content.is_empty() || schema.content.len() > MAX_SCHEMA_BYTES {
        return Err(invalid(
            extension_id,
            "schemas[].content",
            format!("must contain between 1 and {MAX_SCHEMA_BYTES} bytes"),
        ));
    }
    if !valid_media_type(&schema.media_type) {
        return Err(invalid(
            extension_id,
            "schemas[].media_type",
            "must be a lowercase type/subtype without parameters",
        ));
    }
    if schema.media_type.ends_with("json")
        && serde_json::from_slice::<Value>(&schema.content).is_err()
    {
        return Err(invalid(
            extension_id,
            "schemas[].content",
            "declares JSON but is not one complete JSON value",
        ));
    }
    Ok(())
}

fn validate_declared_schemas(
    descriptor: &ExtensionDescriptor,
    schemas: &BTreeMap<String, ExtensionSchema>,
) -> Result<(), RegistryError> {
    let extension_id = descriptor.extension_id.as_str();
    let root = schemas.get(&descriptor.schema_uri).ok_or_else(|| {
        invalid(
            extension_id,
            "schema_uri",
            "is not supplied by the provider",
        )
    })?;
    let root_hash = hex_sha256(&root.content);
    if root_hash != descriptor.schema_hash {
        return Err(invalid(
            extension_id,
            "schema_hash",
            "does not match the registered root schema",
        ));
    }
    for (field, uri) in descriptor
        .actions
        .iter()
        .flat_map(|action| {
            [
                (
                    "actions[].request_schema_uri",
                    action.request_schema_uri.as_str(),
                ),
                (
                    "actions[].response_schema_uri",
                    action.response_schema_uri.as_str(),
                ),
            ]
        })
        .chain(
            descriptor
                .events
                .iter()
                .map(|event| ("events[].schema_uri", event.schema_uri.as_str())),
        )
    {
        if !schemas.contains_key(uri) {
            return Err(invalid(
                extension_id,
                field,
                format!("{uri:?} is not supplied by the provider"),
            ));
        }
    }
    Ok(())
}

fn validate_payload_set(
    extension_id: &str,
    version: &str,
    schema_uri: &str,
    payloads: &[ExtensionPayload],
    max_bytes: usize,
) -> Result<(), String> {
    let mut total = 0usize;
    for payload in payloads {
        total = total
            .checked_add(payload_size(payload))
            .ok_or_else(|| "payload size overflowed".to_string())?;
        if total > max_bytes {
            return Err(format!("payload set exceeds {max_bytes} bytes"));
        }
        validate_payload(extension_id, version, schema_uri, payload, max_bytes)?;
    }
    Ok(())
}

fn validate_payload(
    extension_id: &str,
    version: &str,
    schema_uri: &str,
    payload: &ExtensionPayload,
    max_bytes: usize,
) -> Result<(), String> {
    if payload.extension_id != extension_id {
        return Err("extension_id does not match the registered provider".to_string());
    }
    if payload.schema_version != version {
        return Err("schema_version does not match the registered descriptor".to_string());
    }
    if payload.schema_uri != schema_uri {
        return Err("schema_uri does not match the declared schema".to_string());
    }
    if payload_size(payload) > max_bytes {
        return Err(format!("payload exceeds {max_bytes} bytes"));
    }
    if !valid_media_type(&payload.media_type) {
        return Err("media_type is invalid".to_string());
    }
    match payload.payload.as_ref() {
        Some(extension_payload::Payload::PayloadJson(json)) => {
            if payload.media_type != "application/json" {
                return Err("JSON payloads require application/json".to_string());
            }
            let value: Value = serde_json::from_str(json)
                .map_err(|_| "payload_json is not one complete JSON value".to_string())?;
            validate_json_shape(&value)?;
        }
        Some(extension_payload::Payload::PayloadBytes(_)) => {
            if payload.media_type == "application/json" {
                return Err("application/json payloads must use payload_json".to_string());
            }
        }
        None => return Err("exactly one payload representation is required".to_string()),
    }
    Ok(())
}

fn validate_json_shape(root: &Value) -> Result<(), String> {
    let mut pending = vec![(root, 1usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_JSON_NODES {
            return Err(format!("JSON payload exceeds {MAX_JSON_NODES} nodes"));
        }
        if depth > MAX_JSON_DEPTH {
            return Err(format!("JSON payload exceeds depth {MAX_JSON_DEPTH}"));
        }
        match value {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

fn payload_size(payload: &ExtensionPayload) -> usize {
    match payload.payload.as_ref() {
        Some(extension_payload::Payload::PayloadBytes(bytes)) => bytes.len(),
        Some(extension_payload::Payload::PayloadJson(json)) => json.len(),
        None => 0,
    }
}

fn bounded(extension_id: &str, field: &str, value: &str, max: usize) -> Result<(), RegistryError> {
    if value.trim().is_empty() || value.len() > max {
        return Err(invalid(
            extension_id,
            field,
            format!("must contain between 1 and {max} bytes"),
        ));
    }
    Ok(())
}

fn validate_uri(extension_id: &str, field: &str, value: &str) -> Result<(), RegistryError> {
    bounded(extension_id, field, value, MAX_URI_BYTES)?;
    if value.bytes().any(|byte| byte.is_ascii_whitespace()) || !value.contains(':') {
        return Err(invalid(
            extension_id,
            field,
            "must be an absolute, whitespace-free schema identifier",
        ));
    }
    Ok(())
}

fn validate_local_id(extension_id: &str, field: &str, value: &str) -> Result<(), RegistryError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(invalid(
            extension_id,
            field,
            "must be a bounded ASCII identifier",
        ));
    }
    Ok(())
}

fn valid_namespaced_id(value: &str) -> bool {
    value.len() <= MAX_ID_BYTES
        && value.contains('.')
        && value.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

fn valid_media_type(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'/' | b'.' | b'+' | b'-')
        })
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid(
    extension_id: impl Into<String>,
    field: impl Into<String>,
    reason: impl Into<String>,
) -> RegistryError {
    RegistryError::InvalidDescriptor {
        extension_id: extension_id.into(),
        field: field.into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ddb_api_types::v2::{
        ExtensionActionDescriptor, ExtensionColumnDescriptor, ExtensionPresentationDescriptor,
    };

    use super::*;

    const ROOT_URI: &str = "urn:example:workers:v1";
    const ACTION_URI: &str = "urn:example:workers:rebalance:v1";
    const SCHEMA: &[u8] = br#"{"type":"object"}"#;

    #[derive(Debug)]
    struct ExampleProvider {
        fail_state: bool,
        invocations: Mutex<usize>,
    }

    impl ExampleProvider {
        fn new() -> Self {
            Self {
                fail_state: false,
                invocations: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl ExtensionProvider for ExampleProvider {
        fn descriptor(&self) -> ExtensionDescriptor {
            ExtensionDescriptor {
                extension_id: "org.example.workers".to_string(),
                owner: "Example".to_string(),
                version: "1".to_string(),
                title: "Workers".to_string(),
                description: "Example worker placement".to_string(),
                schema_uri: ROOT_URI.to_string(),
                schema_hash: hex_sha256(SCHEMA),
                required_scopes: vec![PermissionScope::Read as i32],
                actions: vec![ExtensionActionDescriptor {
                    id: "rebalance".to_string(),
                    title: "Rebalance".to_string(),
                    description: None,
                    required_scope: PermissionScope::Control as i32,
                    request_schema_uri: ACTION_URI.to_string(),
                    response_schema_uri: ACTION_URI.to_string(),
                    idempotent: true,
                }],
                presentations: vec![ExtensionPresentationDescriptor {
                    id: "placement".to_string(),
                    title: "Placement".to_string(),
                    description: None,
                    kind: ExtensionPresentationKind::Table as i32,
                    columns: vec![ExtensionColumnDescriptor {
                        id: "worker".to_string(),
                        title: "Worker".to_string(),
                        value_type: Some("string".to_string()),
                    }],
                    action_id: None,
                }],
                minimum_api_version: Some("v2".to_string()),
                ..Default::default()
            }
        }

        fn schemas(&self) -> Vec<ExtensionSchema> {
            vec![
                ExtensionSchema {
                    uri: ROOT_URI.to_string(),
                    media_type: "application/schema+json".to_string(),
                    content: SCHEMA.to_vec(),
                },
                ExtensionSchema {
                    uri: ACTION_URI.to_string(),
                    media_type: "application/schema+json".to_string(),
                    content: SCHEMA.to_vec(),
                },
            ]
        }

        fn state(&self) -> Result<Vec<ExtensionPayload>, ProviderError> {
            if self.fail_state {
                return Err(ProviderError::new(ProviderErrorKind::Failed, "private"));
            }
            Ok(vec![payload(ROOT_URI, r#"{"panels":[]}"#)])
        }

        async fn invoke(
            &self,
            invocation: ExtensionInvocation,
        ) -> Result<ExtensionPayload, ProviderError> {
            *self.invocations.lock().unwrap() += 1;
            Ok(invocation.payload)
        }
    }

    fn payload(schema_uri: &str, json: &str) -> ExtensionPayload {
        ExtensionPayload {
            extension_id: "org.example.workers".to_string(),
            schema_version: "1".to_string(),
            schema_uri: schema_uri.to_string(),
            media_type: "application/json".to_string(),
            payload: Some(extension_payload::Payload::PayloadJson(json.to_string())),
        }
    }

    fn invocation() -> ExtensionInvocation {
        ExtensionInvocation {
            extension_id: "org.example.workers".to_string(),
            action_id: "rebalance".to_string(),
            payload: payload(ACTION_URI, r#"{"worker":"alpha"}"#),
            target: Target::default(),
        }
    }

    #[test]
    fn validates_descriptors_schemas_and_state() {
        let registry =
            ExtensionRegistry::new(vec![Arc::new(ExampleProvider::new())], 4096).unwrap();
        assert_eq!(registry.descriptors().len(), 1);
        assert_eq!(registry.collect_states().states.len(), 1);
        assert!(registry.schema("org.example.workers", ROOT_URI).is_some());
    }

    #[test]
    fn rejects_collisions_and_schema_hash_mismatch() {
        let duplicate = ExtensionRegistry::new(
            vec![
                Arc::new(ExampleProvider::new()),
                Arc::new(ExampleProvider::new()),
            ],
            4096,
        )
        .unwrap_err();
        assert!(matches!(duplicate, RegistryError::DuplicateExtension(_)));

        #[derive(Debug)]
        struct BadHash;
        #[async_trait]
        impl ExtensionProvider for BadHash {
            fn descriptor(&self) -> ExtensionDescriptor {
                let mut descriptor = ExampleProvider::new().descriptor();
                descriptor.schema_hash = "0".repeat(64);
                descriptor
            }
            fn schemas(&self) -> Vec<ExtensionSchema> {
                ExampleProvider::new().schemas()
            }
        }
        assert!(matches!(
            ExtensionRegistry::new(vec![Arc::new(BadHash)], 4096),
            Err(RegistryError::InvalidDescriptor { field, .. }) if field == "schema_hash"
        ));
    }

    #[test]
    fn isolates_provider_state_failure() {
        let provider = ExampleProvider {
            fail_state: true,
            invocations: Mutex::new(0),
        };
        let registry = ExtensionRegistry::new(vec![Arc::new(provider)], 4096).unwrap();
        let collection = registry.collect_states();
        assert!(collection.states.is_empty());
        assert_eq!(
            collection.failures[0].kind,
            ExtensionStateFailureKind::ProviderFailed
        );
    }

    #[tokio::test]
    async fn validates_action_request_and_result() {
        let registry =
            ExtensionRegistry::new(vec![Arc::new(ExampleProvider::new())], 4096).unwrap();
        let result = registry.invoke(invocation(), 4096).await.unwrap();
        assert_eq!(result.schema_uri, ACTION_URI);

        let mut invalid = invocation();
        invalid.payload.schema_uri = ROOT_URI.to_string();
        assert!(matches!(
            registry.invoke(invalid, 4096).await,
            Err(InvocationError::InvalidPayload(_))
        ));
    }

    #[test]
    fn rejects_unbounded_or_mismatched_dynamic_payloads() {
        let registry = ExtensionRegistry::new(vec![Arc::new(ExampleProvider::new())], 8).unwrap();
        assert!(matches!(
            registry.validate_invocation(&invocation()),
            Err(InvocationError::InvalidPayload(_))
        ));

        let mut bytes_as_json = invocation();
        bytes_as_json.payload.payload = Some(extension_payload::Payload::PayloadBytes(vec![1]));
        assert!(matches!(
            ExtensionRegistry::new(vec![Arc::new(ExampleProvider::new())], 4096)
                .unwrap()
                .validate_invocation(&bytes_as_json),
            Err(InvocationError::InvalidPayload(_))
        ));
    }
}
