//! Builds the canonical v2 operation registry from Protobuf descriptors plus
//! the small policy surface that Protobuf cannot express.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::Path,
};

use anyhow::{bail, Context, Result};
use prost_types::{EnumDescriptorProto, FileDescriptorProto, FileDescriptorSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const API_PACKAGE: &str = "ddb.api.v2";
const POLICY_PATH: &str = "proto/ddb/api/v2/operation_policy.json";
const ERROR_ENUM: &str = "DdbErrorCode";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Permission {
    Public,
    Read,
    Control,
    Admin,
}

impl Permission {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Read => "read",
            Self::Control => "control",
            Self::Admin => "admin",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HttpPolicy {
    pub(crate) base_path: String,
    pub(crate) method: String,
    pub(crate) request_content_type: String,
    pub(crate) unary_response_content_type: String,
    pub(crate) stream_response_content_type: String,
    pub(crate) success_status: u16,
    pub(crate) max_request_bytes: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServicePolicy {
    default_scope: Permission,
    method_scopes: BTreeMap<String, Permission>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StreamPolicy {
    pub(crate) channel: String,
    pub(crate) lane: String,
    pub(crate) heartbeat_seconds: u64,
    pub(crate) cursor_replay: String,
    pub(crate) ordering: String,
    pub(crate) replay_limits: String,
    pub(crate) backpressure: String,
    pub(crate) loss_signaling: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErrorPolicy {
    pub(crate) code: String,
    pub(crate) status: u16,
    pub(crate) description: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyDocument {
    schema_version: u32,
    http: HttpPolicy,
    services: BTreeMap<String, ServicePolicy>,
    streams: BTreeMap<String, StreamPolicy>,
    errors: Vec<ErrorPolicy>,
}

#[derive(Clone, Debug)]
pub(crate) struct ServiceSpec {
    pub(crate) name: String,
    pub(crate) full_name: String,
    pub(crate) description: String,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationSpec {
    pub(crate) key: String,
    pub(crate) operation_id: String,
    pub(crate) service: String,
    pub(crate) method: String,
    pub(crate) protobuf_method: String,
    pub(crate) handler: String,
    pub(crate) path: String,
    pub(crate) input_type: String,
    pub(crate) output_type: String,
    pub(crate) permission: Permission,
    pub(crate) server_streaming: bool,
    pub(crate) description: String,
    pub(crate) stream: Option<StreamPolicy>,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationRegistry {
    pub(crate) http: HttpPolicy,
    pub(crate) services: Vec<ServiceSpec>,
    pub(crate) operations: Vec<OperationSpec>,
    pub(crate) errors: Vec<ErrorPolicy>,
}

impl OperationRegistry {
    pub(crate) fn load(workspace: &Path, descriptors: &FileDescriptorSet) -> Result<Self> {
        let policy_path = workspace.join(POLICY_PATH);
        let policy: PolicyDocument = serde_json::from_slice(
            &fs::read(&policy_path)
                .with_context(|| format!("read operation policy {}", policy_path.display()))?,
        )
        .with_context(|| format!("parse operation policy {}", policy_path.display()))?;
        Self::build(policy, descriptors)
    }

    fn build(policy: PolicyDocument, descriptors: &FileDescriptorSet) -> Result<Self> {
        if policy.schema_version != 1 {
            bail!(
                "unsupported operation policy schema {}; expected 1",
                policy.schema_version
            );
        }
        validate_http_policy(&policy.http)?;

        let files = api_files(descriptors).collect::<Vec<_>>();
        let descriptor_services = files
            .iter()
            .flat_map(|file| {
                file.service
                    .iter()
                    .map(|service| service.name().to_string())
            })
            .collect::<BTreeSet<_>>();
        let policy_services = policy.services.keys().cloned().collect::<BTreeSet<_>>();
        if descriptor_services != policy_services {
            bail!(
                "operation policy services disagree with Protobuf: descriptor={descriptor_services:?}, policy={policy_services:?}"
            );
        }

        let mut services = Vec::new();
        let mut operations = Vec::new();
        let mut operation_keys = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut expected_streams = BTreeSet::new();

        for file in files {
            for (service_index, service) in file.service.iter().enumerate() {
                let service_name = service.name().to_string();
                let full_service = format!("{}.{}", file.package(), service_name);
                let service_policy = policy
                    .services
                    .get(&service_name)
                    .context("validated service policy is missing")?;
                let method_names = service
                    .method
                    .iter()
                    .map(|method| method.name().to_string())
                    .collect::<BTreeSet<_>>();
                for override_name in service_policy.method_scopes.keys() {
                    if !method_names.contains(override_name) {
                        bail!(
                            "operation policy override {service_name}.{override_name} does not name a Protobuf method"
                        );
                    }
                }

                services.push(ServiceSpec {
                    name: service_name.clone(),
                    full_name: full_service.clone(),
                    description: descriptor_comment(file, &[6, service_index as i32])
                        .unwrap_or_else(|| format!("{service_name} API operations.")),
                });

                for (method_index, method) in service.method.iter().enumerate() {
                    if method.client_streaming() {
                        bail!(
                            "HTTP/ProtoJSON does not support client-streaming method {full_service}.{}",
                            method.name()
                        );
                    }
                    let method_name = method.name().to_string();
                    let key = format!("{service_name}.{method_name}");
                    let path = format!("{}/{full_service}/{method_name}", policy.http.base_path);
                    if !operation_keys.insert(key.clone()) {
                        bail!("duplicate public operation {key}");
                    }
                    if !paths.insert(path.clone()) {
                        bail!("duplicate HTTP operation path {path}");
                    }

                    let stream = policy.streams.get(&key).cloned();
                    if method.server_streaming() {
                        expected_streams.insert(key.clone());
                        if stream.is_none() {
                            bail!("server-streaming method {key} has no stream policy");
                        }
                    } else if stream.is_some() {
                        bail!("unary method {key} unexpectedly has a stream policy");
                    }
                    let permission = service_policy
                        .method_scopes
                        .get(&method_name)
                        .copied()
                        .unwrap_or(service_policy.default_scope);
                    let description = descriptor_comment(
                        file,
                        &[6, service_index as i32, 2, method_index as i32],
                    )
                    .unwrap_or_else(|| format!("{service_name}.{method_name}"));

                    operations.push(OperationSpec {
                        key: key.clone(),
                        operation_id: format!("{service_name}_{method_name}"),
                        service: service_name.clone(),
                        method: method_name.clone(),
                        protobuf_method: format!("/{full_service}/{method_name}"),
                        handler: format!("v2_{}", snake_case(&method_name)),
                        path,
                        input_type: method.input_type().to_string(),
                        output_type: method.output_type().to_string(),
                        permission,
                        server_streaming: method.server_streaming(),
                        description,
                        stream,
                    });
                }
            }
        }

        let configured_streams = policy.streams.keys().cloned().collect::<BTreeSet<_>>();
        if expected_streams != configured_streams {
            bail!(
                "stream policies disagree with Protobuf: descriptor={expected_streams:?}, policy={configured_streams:?}"
            );
        }
        validate_stream_policies(&policy.streams)?;

        let errors = ordered_error_policies(descriptors, policy.errors)?;

        Ok(Self {
            http: policy.http,
            services,
            operations,
            errors,
        })
    }

    pub(crate) fn document(&self) -> Value {
        let error_statuses = self
            .errors
            .iter()
            .map(|error| error.status)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        json!({
            "schemaVersion": 1,
            "source": {
                "protobufPackage": API_PACKAGE,
                "policy": POLICY_PATH,
            },
            "http": self.http,
            "services": self.services.iter().map(|service| json!({
                "name": service.name,
                "protobufService": service.full_name,
                "description": service.description,
            })).collect::<Vec<_>>(),
            "operations": self.operations.iter().map(|operation| {
                let mut value = json!({
                    "key": operation.key,
                    "operationId": operation.operation_id,
                    "protobufMethod": operation.protobuf_method,
                    "httpMethod": self.http.method,
                    "path": operation.path,
                    "requestType": operation.input_type,
                    "responseType": operation.output_type,
                    "permission": operation.permission,
                    "serverStreaming": operation.server_streaming,
                    "description": operation.description,
                    "successStatus": self.http.success_status,
                    "errorStatuses": error_statuses,
                });
                if let Some(stream) = &operation.stream {
                    value["stream"] = serde_json::to_value(stream)
                        .expect("serializing a validated stream policy cannot fail");
                }
                value
            }).collect::<Vec<_>>(),
            "errors": self.errors,
        })
    }

    pub(crate) fn runtime_source(&self) -> Result<String> {
        let mut output = String::from(
            "// @generated by ddb-api-codegen from Protobuf and operation_policy.json.\n\
             // Do not edit. Runtime routing and documentation consume the same registry.\n\n",
        );
        writeln!(
            output,
            "pub(crate) const V2_MAX_REQUEST_BYTES: usize = {};",
            self.http.max_request_bytes
        )?;
        writeln!(
            output,
            "const V2_SUCCESS_STATUS: u16 = {};",
            self.http.success_status
        )?;
        writeln!(
            output,
            "const V2_UNARY_CONTENT_TYPE: &str = {};",
            rust_string(&self.http.unary_response_content_type)?
        )?;
        writeln!(
            output,
            "const V2_STREAM_CONTENT_TYPE: &str = {};",
            rust_string(&self.http.stream_response_content_type)?
        )?;
        output.push('\n');

        for operation in &self.operations {
            writeln!(
                output,
                "const {}: &str = {};",
                path_constant(operation),
                rust_string(&operation.path)?
            )?;
            if let Some(stream) = &operation.stream {
                writeln!(
                    output,
                    "const {}: Duration = Duration::from_secs({});",
                    heartbeat_constant(operation),
                    stream.heartbeat_seconds
                )?;
            }
        }

        output.push_str(
            "fn v2_error_status(code: v2::DdbErrorCode) -> StatusCode {\n    match code {\n",
        );
        for error in &self.errors {
            writeln!(
                output,
                "        v2::DdbErrorCode::{} => StatusCode::from_u16({}).expect(\"validated v2 HTTP status\"),",
                rust_error_variant(&error.code)?,
                error.status
            )?;
        }
        output.push_str("    }\n}\n\n");

        output.push_str(
            "fn v2_contract_router(authorization: &Arc<ApiAuthorization>) -> Router<ApiState> {\n",
        );
        for permission in [
            Permission::Public,
            Permission::Read,
            Permission::Control,
            Permission::Admin,
        ] {
            writeln!(output, "    let {} = Router::new()", permission.as_str())?;
            for operation in self
                .operations
                .iter()
                .filter(|operation| operation.permission == permission)
            {
                writeln!(
                    output,
                    "        .route({}, post({}))",
                    path_constant(operation),
                    operation.handler
                )?;
            }
            match permission {
                Permission::Public => output.push_str("        ;\n"),
                Permission::Read => output.push_str(
                    "        .route_layer(middleware::from_fn_with_state(\n            Arc::clone(authorization),\n            require_read,\n        ));\n",
                ),
                Permission::Control => output.push_str(
                    "        .route_layer(middleware::from_fn_with_state(\n            Arc::clone(authorization),\n            require_control,\n        ));\n",
                ),
                Permission::Admin => output.push_str(
                    "        .route_layer(middleware::from_fn_with_state(\n            Arc::clone(authorization),\n            require_admin,\n        ));\n",
                ),
            }
        }
        output.push_str("\n    public.merge(read).merge(control).merge(admin)\n}\n");
        Ok(output)
    }
}

fn validate_http_policy(http: &HttpPolicy) -> Result<()> {
    if !http.base_path.starts_with('/') || http.base_path.ends_with('/') {
        bail!("HTTP basePath must start with '/' and must not end with '/'");
    }
    if http.method != "post" {
        bail!("the v2 HTTP/ProtoJSON binding currently requires POST");
    }
    if http.request_content_type != "application/json"
        || http.unary_response_content_type != "application/json"
        || http.stream_response_content_type != "application/x-ndjson"
    {
        bail!("operation policy content types disagree with the implemented transport");
    }
    if !(200..300).contains(&http.success_status) {
        bail!("HTTP successStatus must be a 2xx status");
    }
    if http.max_request_bytes == 0 {
        bail!("HTTP maxRequestBytes must be non-zero");
    }
    Ok(())
}

fn validate_stream_policies(streams: &BTreeMap<String, StreamPolicy>) -> Result<()> {
    let mut channels = BTreeSet::new();
    for (key, stream) in streams {
        if stream.channel.is_empty() || !channels.insert(stream.channel.clone()) {
            bail!("stream {key} has an empty or duplicate AsyncAPI channel");
        }
        if stream.lane != "state" && stream.lane != "output" {
            bail!("stream {key} has unsupported lane {}", stream.lane);
        }
        if stream.heartbeat_seconds == 0 {
            bail!("stream {key} heartbeatSeconds must be non-zero");
        }
        for (label, value) in [
            ("cursorReplay", &stream.cursor_replay),
            ("ordering", &stream.ordering),
            ("replayLimits", &stream.replay_limits),
            ("backpressure", &stream.backpressure),
            ("lossSignaling", &stream.loss_signaling),
        ] {
            if value.trim().is_empty() {
                bail!("stream {key} has empty {label} semantics");
            }
        }
    }
    Ok(())
}

fn ordered_error_policies(
    descriptors: &FileDescriptorSet,
    configured: Vec<ErrorPolicy>,
) -> Result<Vec<ErrorPolicy>> {
    let descriptor = api_files(descriptors)
        .flat_map(|file| file.enum_type.iter())
        .find(|descriptor| descriptor.name() == ERROR_ENUM)
        .context("canonical DdbErrorCode descriptor is missing")?;
    let mut by_code = BTreeMap::new();
    for error in configured {
        if !(400..600).contains(&error.status) {
            bail!("{} has invalid HTTP status {}", error.code, error.status);
        }
        if error.description.trim().is_empty() {
            bail!("{} has an empty description", error.code);
        }
        let code = error.code.clone();
        if by_code.insert(code.clone(), error).is_some() {
            bail!("duplicate error policy for {code}");
        }
    }

    let descriptor_codes = enum_values(descriptor);
    let configured_codes = by_code.keys().cloned().collect::<BTreeSet<_>>();
    if descriptor_codes != configured_codes {
        bail!(
            "HTTP error policies disagree with DdbErrorCode: descriptor={descriptor_codes:?}, policy={configured_codes:?}"
        );
    }
    descriptor
        .value
        .iter()
        .map(|value| {
            by_code
                .remove(value.name())
                .context("validated error policy is missing")
        })
        .collect()
}

fn enum_values(descriptor: &EnumDescriptorProto) -> BTreeSet<String> {
    descriptor
        .value
        .iter()
        .map(|value| value.name().to_string())
        .collect()
}

fn descriptor_comment(file: &FileDescriptorProto, path: &[i32]) -> Option<String> {
    let location = file
        .source_code_info
        .as_ref()?
        .location
        .iter()
        .find(|location| location.path == path)?;
    let raw = location
        .leading_comments
        .as_deref()
        .or(location.trailing_comments.as_deref())?;
    let normalized = raw
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn api_files(descriptor_set: &FileDescriptorSet) -> impl Iterator<Item = &FileDescriptorProto> {
    descriptor_set
        .file
        .iter()
        .filter(|file| file.package() == API_PACKAGE)
}

fn snake_case(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    for (index, character) in chars.iter().copied().enumerate() {
        if character.is_uppercase()
            && index > 0
            && (chars[index - 1].is_lowercase()
                || chars.get(index + 1).is_some_and(|next| next.is_lowercase()))
        {
            output.push('_');
        }
        output.extend(character.to_lowercase());
    }
    output
}

fn path_constant(operation: &OperationSpec) -> String {
    format!(
        "V2_{}_{}_PATH",
        snake_case(&operation.service).to_uppercase(),
        snake_case(&operation.method).to_uppercase()
    )
}

pub(crate) fn heartbeat_constant(operation: &OperationSpec) -> String {
    format!(
        "V2_{}_{}_HEARTBEAT",
        snake_case(&operation.service).to_uppercase(),
        snake_case(&operation.method).to_uppercase()
    )
}

fn rust_error_variant(code: &str) -> Result<String> {
    let suffix = code
        .strip_prefix("DDB_ERROR_CODE_")
        .with_context(|| format!("error policy code {code} has no DDB_ERROR_CODE_ prefix"))?;
    Ok(suffix
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| {
                    first
                        .to_uppercase()
                        .chain(chars.flat_map(char::to_lowercase))
                        .collect::<String>()
                })
                .unwrap_or_default()
        })
        .collect())
}

fn rust_string(value: &str) -> Result<String> {
    serde_json::to_string(value).context("serialize Rust string literal")
}

#[cfg(test)]
mod tests {
    use super::snake_case;

    #[test]
    fn handler_names_follow_rpc_names() {
        assert_eq!(snake_case("GetServerInfo"), "get_server_info");
        assert_eq!(snake_case("DDBStatus"), "ddb_status");
        assert_eq!(snake_case("SubscribeOutput"), "subscribe_output");
    }
}
