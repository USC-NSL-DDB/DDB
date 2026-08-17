mod support;

use std::io::{BufRead, BufReader};

use jsonschema::Draft;
use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN};

const OPENAPI: &str = include_str!("../../docs/api/generated/openapi-v2.json");
const ASYNCAPI: &str = include_str!("../../docs/api/generated/asyncapi-v2.json");
const RPC_ROOT: &str = "/api/v2/rpc";

fn rpc(service: &str, method: &str) -> String {
    format!("{RPC_ROOT}/ddb.api.v2.{service}/{method}")
}

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "spec-conformance",
        alias: "schema-runtime",
        hash: "spec-conformance-group",
        pid: 913,
        start_delay_ms: 0,
        source_file: "tests/api_v2_spec_conformance.rs",
        source_line: 20,
        function: "mock_session",
        exit_on_continue: false,
    }
}

fn assert_schema_valid(document: &Value, schema: &Value, instance: &Value, label: &str) {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": schema["$ref"],
        "components": document["components"],
    });
    let validator = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .unwrap_or_else(|error| panic!("{label} schema should compile: {error}"));
    let errors = validator
        .iter_errors(instance)
        .map(|error| {
            format!(
                "{} at instance {} / schema {}",
                error,
                error.instance_path(),
                error.schema_path()
            )
        })
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{label} failed generated JSON Schema validation:\n{}\ninstance: {instance}",
        errors.join("\n")
    );
}

fn resolve_local_reference<'a>(document: &'a Value, value: &'a Value) -> &'a Value {
    let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
        return value;
    };
    let pointer = reference
        .strip_prefix('#')
        .unwrap_or_else(|| panic!("only local references are expected: {reference}"));
    document
        .pointer(pointer)
        .unwrap_or_else(|| panic!("unresolved local reference {reference}"))
}

fn assert_openapi_response(
    openapi: &Value,
    path: &str,
    status: StatusCode,
    body: &Value,
    label: &str,
) {
    let response = &openapi["paths"][path]["post"]["responses"][status.as_str()];
    let response = resolve_local_reference(openapi, response);
    let schema = &response["content"]["application/json"]["schema"];
    assert_schema_valid(openapi, schema, body, label);
}

#[test]
fn every_embedded_openapi_and_asyncapi_example_matches_its_schema() {
    let openapi: Value = serde_json::from_str(OPENAPI).expect("OpenAPI should be JSON");
    for (path, item) in openapi["paths"]
        .as_object()
        .expect("OpenAPI paths should be an object")
    {
        let media = &item["post"]["requestBody"]["content"]["application/json"];
        let example = media
            .get("example")
            .unwrap_or_else(|| panic!("{path} must have a request example"));
        assert_schema_valid(
            &openapi,
            &media["schema"],
            example,
            &format!("{path} request example"),
        );
    }

    let asyncapi: Value = serde_json::from_str(ASYNCAPI).expect("AsyncAPI should be JSON");
    for (name, message) in asyncapi["components"]["messages"]
        .as_object()
        .expect("AsyncAPI messages should be an object")
    {
        let example = &message["examples"][0]["payload"];
        assert_schema_valid(
            &asyncapi,
            &message["payload"],
            example,
            &format!("{name} message example"),
        );
    }
}

#[test]
fn captured_runtime_payloads_match_generated_openapi_and_asyncapi_schemas() {
    let openapi: Value = serde_json::from_str(OPENAPI).expect("OpenAPI should be JSON");
    let asyncapi: Value = serde_json::from_str(ASYNCAPI).expect("AsyncAPI should be JSON");
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let server_info_path = rpc("DebuggerService", "GetServerInfo");
    let (status, server_info) = ddb.api_post_json(&server_info_path, &json!({}));
    assert_eq!(status, StatusCode::OK, "{server_info:?}");
    assert_openapi_response(
        &openapi,
        &server_info_path,
        status,
        &server_info,
        "GetServerInfo runtime response",
    );

    let capabilities_path = rpc("DebuggerService", "GetCapabilities");
    let (status, capabilities) =
        ddb.api_post_json_with_bearer(&capabilities_path, &json!({}), V2_TEST_READ_TOKEN);
    assert_eq!(status, StatusCode::OK, "{capabilities:?}");
    assert_eq!(
        capabilities["capabilities"]["limits"]["maxRequestBytes"]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok()),
        openapi["x-ddb-max-request-bytes"].as_u64(),
        "GetCapabilities must advertise the generated HTTP request limit"
    );
    assert_openapi_response(
        &openapi,
        &capabilities_path,
        status,
        &capabilities,
        "GetCapabilities runtime response",
    );

    let snapshot_path = rpc("DebuggerService", "GetSnapshot");
    let (status, snapshot) = ddb.api_post_json_with_bearer(
        &snapshot_path,
        &json!({"sections": ["SNAPSHOT_SECTION_TOPOLOGY"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{snapshot:?}");
    assert_openapi_response(
        &openapi,
        &snapshot_path,
        status,
        &snapshot,
        "GetSnapshot runtime response",
    );
    let thread_id = snapshot["snapshot"]["threads"][0]["threadId"]
        .as_str()
        .expect("snapshot should expose a thread")
        .to_string();

    let resolve_path = rpc("DebuggerService", "ResolveSource");
    let (status, not_found) = ddb.api_post_json_with_bearer(
        &resolve_path,
        &json!({"location": {"path": "/definitely/not/a/source.rs", "line": 1}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::NOT_FOUND, "{not_found:?}");
    assert_openapi_response(
        &openapi,
        &resolve_path,
        status,
        &not_found,
        "DdbError runtime response",
    );

    let state_path = rpc("DdbEventService", "SubscribeStateEvents");
    let state_stream = ddb.api_post_stream_with_bearer(
        &state_path,
        &json!({
            "filter": {
                "kinds": ["STATE_EVENT_KIND_OPERATION_CHANGED"],
                "resourceKinds": ["RESOURCE_KIND_OPERATION"]
            }
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(state_stream.status(), StatusCode::OK);

    let execute_path = rpc("DebuggerControlService", "Execute");
    let (status, admission) = ddb.api_post_json_with_bearer(
        &execute_path,
        &json!({
            "context": {"idempotencyKey": "spec-conformance-next"},
            "target": {"thread": {"threadId": thread_id}},
            "action": "EXECUTION_ACTION_NEXT"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admission:?}");
    assert_openapi_response(
        &openapi,
        &execute_path,
        status,
        &admission,
        "Execute runtime response",
    );

    let mut state_stream = BufReader::new(state_stream);
    let mut line = String::new();
    state_stream
        .read_line(&mut line)
        .expect("state event should be readable");
    let state_event: Value =
        serde_json::from_str(line.trim()).expect("state stream should contain ProtoJSON");
    assert_schema_valid(
        &asyncapi,
        &asyncapi["components"]["messages"]["StateEvent"]["payload"],
        &state_event,
        "StateEvent runtime message",
    );
}
