//! A self-contained DDB extension provider.
//!
//! This crate intentionally depends on no DDB backend internals. A framework
//! adapter can register `SampleWorkersExtension` as an `ExtensionProvider`, and
//! any frontend can render its standard presentation document from public API
//! descriptors alone.

use std::{collections::BTreeMap, sync::Mutex};

use async_trait::async_trait;
use ddb_api_extension::{
    ExtensionInvocation, ExtensionProvider, ExtensionSchema, ProviderError, ProviderErrorKind,
};
use ddb_api_types::v2::{
    extension_payload, ExtensionActionDescriptor, ExtensionColumnDescriptor, ExtensionDescriptor,
    ExtensionPayload, ExtensionPresentationDescriptor, ExtensionPresentationKind, PermissionScope,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const EXTENSION_ID: &str = "org.ddb.example.workers";
pub const ROOT_SCHEMA_URI: &str = "urn:ddb:example:workers:v1";
pub const MOVE_REQUEST_SCHEMA_URI: &str = "urn:ddb:example:workers:move-request:v1";
pub const MOVE_RESULT_SCHEMA_URI: &str = "urn:ddb:example:workers:move-result:v1";
pub const MOVE_ACTION_ID: &str = "move_worker";

const ROOT_SCHEMA: &[u8] = include_bytes!("../schemas/workers-v1.schema.json");
const MOVE_REQUEST_SCHEMA: &[u8] = include_bytes!("../schemas/move-request-v1.schema.json");
const MOVE_RESULT_SCHEMA: &[u8] = include_bytes!("../schemas/move-result-v1.schema.json");

#[derive(Debug)]
pub struct SampleWorkersExtension {
    placements: Mutex<BTreeMap<String, String>>,
}

impl Default for SampleWorkersExtension {
    fn default() -> Self {
        Self::new([
            ("alpha".to_string(), "session-7".to_string()),
            ("beta".to_string(), "session-8".to_string()),
        ])
    }
}

impl SampleWorkersExtension {
    pub fn new(placements: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            placements: Mutex::new(placements.into_iter().collect()),
        }
    }
}

#[async_trait]
impl ExtensionProvider for SampleWorkersExtension {
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            extension_id: EXTENSION_ID.to_string(),
            owner: "DDB examples".to_string(),
            version: "1.0.0".to_string(),
            title: "Worker placement".to_string(),
            description: "Independent sample extension with generic presentations".to_string(),
            schema_uri: ROOT_SCHEMA_URI.to_string(),
            schema_hash: sha256(ROOT_SCHEMA),
            required_scopes: vec![PermissionScope::Read as i32],
            actions: vec![ExtensionActionDescriptor {
                id: MOVE_ACTION_ID.to_string(),
                title: "Move worker".to_string(),
                description: Some("Assign a worker to another session".to_string()),
                required_scope: PermissionScope::Control as i32,
                request_schema_uri: MOVE_REQUEST_SCHEMA_URI.to_string(),
                response_schema_uri: MOVE_RESULT_SCHEMA_URI.to_string(),
                idempotent: true,
            }],
            events: Vec::new(),
            presentations: vec![
                ExtensionPresentationDescriptor {
                    id: "placement".to_string(),
                    title: "Placement".to_string(),
                    description: Some("Current worker ownership".to_string()),
                    kind: ExtensionPresentationKind::Table as i32,
                    columns: vec![column("worker", "Worker"), column("session", "Session")],
                    action_id: None,
                },
                presentation("summary", "Summary", ExtensionPresentationKind::KeyValue),
                presentation("topology", "Topology", ExtensionPresentationKind::Tree),
                presentation("status", "Status", ExtensionPresentationKind::Text),
                ExtensionPresentationDescriptor {
                    id: "move".to_string(),
                    title: "Move worker".to_string(),
                    description: Some("Invoke through InvokeExtensionAction".to_string()),
                    kind: ExtensionPresentationKind::Action as i32,
                    columns: Vec::new(),
                    action_id: Some(MOVE_ACTION_ID.to_string()),
                },
            ],
            minimum_api_version: Some("v2".to_string()),
            maximum_api_version: None,
        }
    }

    fn schemas(&self) -> Vec<ExtensionSchema> {
        [
            (ROOT_SCHEMA_URI, ROOT_SCHEMA),
            (MOVE_REQUEST_SCHEMA_URI, MOVE_REQUEST_SCHEMA),
            (MOVE_RESULT_SCHEMA_URI, MOVE_RESULT_SCHEMA),
        ]
        .into_iter()
        .map(|(uri, content)| ExtensionSchema {
            uri: uri.to_string(),
            media_type: "application/schema+json".to_string(),
            content: content.to_vec(),
        })
        .collect()
    }

    fn state(&self) -> Result<Vec<ExtensionPayload>, ProviderError> {
        let placements = self
            .placements
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let rows = placements
            .iter()
            .map(|(worker, session)| vec![worker.clone(), session.clone()])
            .collect::<Vec<_>>();
        let nodes = placements
            .iter()
            .map(|(worker, session)| {
                serde_json::json!({"label": worker, "value": session, "children": []})
            })
            .collect::<Vec<_>>();
        let document = serde_json::json!({
            "presentations": {
                "placement": {"rows": rows},
                "summary": {"entries": [
                    {"key": "workers", "value": placements.len().to_string()}
                ]},
                "topology": {"nodes": [
                    {"label": "workers", "children": nodes}
                ]},
                "status": {"text": "sample provider ready"},
                "move": {"enabled": true}
            }
        });
        Ok(vec![json_payload(ROOT_SCHEMA_URI, document.to_string())])
    }

    async fn invoke(
        &self,
        invocation: ExtensionInvocation,
    ) -> Result<ExtensionPayload, ProviderError> {
        if invocation.action_id != MOVE_ACTION_ID {
            return Err(ProviderError::unsupported());
        }
        let request = match invocation.payload.payload {
            Some(extension_payload::Payload::PayloadJson(json)) => {
                serde_json::from_str::<MoveWorkerRequest>(&json).map_err(|_| {
                    ProviderError::new(
                        ProviderErrorKind::InvalidRequest,
                        "request does not match the move-worker schema",
                    )
                })?
            }
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorKind::InvalidRequest,
                    "move-worker requires JSON",
                ));
            }
        };
        if request.worker.trim().is_empty() || request.session.trim().is_empty() {
            return Err(ProviderError::new(
                ProviderErrorKind::InvalidRequest,
                "worker and session must be non-empty",
            ));
        }
        let previous_session = self
            .placements
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(request.worker.clone(), request.session.clone());
        let result = MoveWorkerResult {
            worker: request.worker,
            previous_session,
            session: request.session,
        };
        let json = serde_json::to_string(&result).map_err(|_| {
            ProviderError::new(ProviderErrorKind::Failed, "result serialization failed")
        })?;
        Ok(json_payload(MOVE_RESULT_SCHEMA_URI, json))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveWorkerRequest {
    worker: String,
    session: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MoveWorkerResult {
    worker: String,
    previous_session: Option<String>,
    session: String,
}

pub fn move_worker_payload(worker: &str, session: &str) -> ExtensionPayload {
    json_payload(
        MOVE_REQUEST_SCHEMA_URI,
        serde_json::json!({"worker": worker, "session": session}).to_string(),
    )
}

fn json_payload(schema_uri: &str, json: String) -> ExtensionPayload {
    ExtensionPayload {
        extension_id: EXTENSION_ID.to_string(),
        schema_version: "1.0.0".to_string(),
        schema_uri: schema_uri.to_string(),
        media_type: "application/json".to_string(),
        payload: Some(extension_payload::Payload::PayloadJson(json)),
    }
}

fn column(id: &str, title: &str) -> ExtensionColumnDescriptor {
    ExtensionColumnDescriptor {
        id: id.to_string(),
        title: title.to_string(),
        value_type: Some("string".to_string()),
    }
}

fn presentation(
    id: &str,
    title: &str,
    kind: ExtensionPresentationKind,
) -> ExtensionPresentationDescriptor {
    ExtensionPresentationDescriptor {
        id: id.to_string(),
        title: title.to_string(),
        description: None,
        kind: kind as i32,
        columns: Vec::new(),
        action_id: None,
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ddb_api_extension::{ExtensionInvocation, ExtensionRegistry};
    use ddb_api_types::v2::Target;

    use super::*;

    #[tokio::test]
    async fn sample_is_registry_valid_and_action_updates_generic_state() {
        let registry =
            ExtensionRegistry::new(vec![Arc::new(SampleWorkersExtension::default())], 64 * 1024)
                .unwrap();
        assert_eq!(registry.descriptors()[0].presentations.len(), 5);
        registry
            .invoke(
                ExtensionInvocation {
                    extension_id: EXTENSION_ID.to_string(),
                    action_id: MOVE_ACTION_ID.to_string(),
                    payload: move_worker_payload("alpha", "session-9"),
                    target: Target::default(),
                },
                64 * 1024,
            )
            .await
            .unwrap();
        let state = registry.collect_states();
        let json = match state.states[0].payloads[0].payload.as_ref().unwrap() {
            extension_payload::Payload::PayloadJson(json) => json,
            extension_payload::Payload::PayloadBytes(_) => panic!("expected JSON"),
        };
        assert!(json.contains("session-9"));
    }
}
