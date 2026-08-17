mod support;

use std::{thread, time::Duration};

use reqwest::{
    blocking::Client,
    header::{ACCESS_CONTROL_ALLOW_ORIGIN, CONTENT_ENCODING, ORIGIN, RETRY_AFTER},
    StatusCode,
};
use serde_json::{json, Value};
use support::{DdbProcess, V2_TEST_ADMIN_TOKEN, V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN};

const SERVER_INFO: &str = "/api/v2/rpc/ddb.api.v2.DebuggerService/GetServerInfo";
const CAPABILITIES: &str = "/api/v2/rpc/ddb.api.v2.DebuggerService/GetCapabilities";
const STATE_EVENTS: &str = "/api/v2/rpc/ddb.api.v2.DdbEventService/SubscribeStateEvents";
const SHUTDOWN: &str = "/api/v2/rpc/ddb.api.v2.DdbAdminService/Shutdown";

fn response_json(response: reqwest::blocking::Response) -> (StatusCode, Value) {
    let status = response.status();
    let body = response.json().expect("policy response should be JSON");
    (status, body)
}

#[test]
fn every_generated_v2_method_enforces_its_advertised_scope() {
    let ddb = DdbProcess::spawn_with_v2_auth(&[]);
    let client = Client::new();
    let registry: Value = serde_json::from_str(include_str!(
        "../../docs/api/generated/operation-registry-v2.json"
    ))
    .expect("checked-in operation registry should be valid JSON");
    let openapi: Value =
        serde_json::from_str(include_str!("../../docs/api/generated/openapi-v2.json"))
            .expect("checked-in OpenAPI should be valid JSON");
    let operations = registry["operations"]
        .as_array()
        .expect("registry operations should be an array");
    let paths = openapi["paths"]
        .as_object()
        .expect("OpenAPI paths should be an object");
    assert_eq!(
        paths.len(),
        operations.len(),
        "OpenAPI and runtime registry operation counts disagree"
    );

    for operation in operations {
        let key = operation["key"].as_str().expect("operation key");
        let path = operation["path"].as_str().expect("operation path");
        let permission = operation["permission"]
            .as_str()
            .expect("operation permission");
        let documented = paths
            .get(path)
            .unwrap_or_else(|| panic!("OpenAPI omitted registry path {path}"));
        assert_eq!(documented["post"]["x-ddb-registry-key"], key);
        match permission {
            "public" => assert!(documented["post"].get("x-ddb-required-scope").is_none()),
            "read" | "control" | "admin" => {
                assert_eq!(documented["post"]["x-ddb-required-scope"], permission)
            }
            other => panic!("unknown registry permission {other} for {path}"),
        }

        let endpoint = format!("{}{}", ddb.api_endpoint(), path);
        let unauthenticated = client
            .post(&endpoint)
            .json(&json!({}))
            .send()
            .unwrap_or_else(|error| {
                panic!("{path} should answer without transport failure: {error}")
            });
        if permission == "public" {
            assert_eq!(
                unauthenticated.status(),
                StatusCode::OK,
                "public method {path} should be callable without credentials"
            );
            continue;
        }
        assert_eq!(
            unauthenticated.status(),
            StatusCode::UNAUTHORIZED,
            "protected method {path} should reject missing credentials"
        );

        let (token, denied_token) = match permission {
            "read" => (V2_TEST_READ_TOKEN, None),
            "control" => (V2_TEST_CONTROL_TOKEN, Some(V2_TEST_READ_TOKEN)),
            "admin" => (V2_TEST_ADMIN_TOKEN, Some(V2_TEST_CONTROL_TOKEN)),
            _ => unreachable!(),
        };
        if let Some(denied_token) = denied_token {
            let denied = client
                .post(&endpoint)
                .bearer_auth(denied_token)
                .json(&json!({}))
                .send()
                .unwrap_or_else(|error| panic!("{path} scope denial should answer: {error}"));
            assert_eq!(
                denied.status(),
                StatusCode::FORBIDDEN,
                "method {path} accepted a credential below its registry scope"
            );
        }

        let allowed = client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .unwrap_or_else(|error| panic!("{path} authorized request should answer: {error}"));
        assert_ne!(
            allowed.status(),
            StatusCode::UNAUTHORIZED,
            "method {path} rejected its registry scope"
        );
        assert_ne!(
            allowed.status(),
            StatusCode::FORBIDDEN,
            "method {path} rejected its registry scope"
        );
    }
}
#[test]
fn remote_listener_exposes_only_v2_and_requires_the_configured_auth_scope() {
    let ddb = DdbProcess::spawn_with_v2_conf(
        &[],
        "  api_server_bind: 0.0.0.0\n  api_tls_terminated_by_trusted_proxy: true",
    );
    let client = Client::new();

    let legacy = client
        .get(format!("{}/status", ddb.api_endpoint()))
        .send()
        .expect("remote listener should answer");
    assert_eq!(legacy.status(), StatusCode::NOT_FOUND);

    let server_info = client
        .post(format!("{}{}", ddb.api_endpoint(), SERVER_INFO))
        .json(&json!({}))
        .send()
        .expect("public server information should answer");
    assert_eq!(server_info.status(), StatusCode::OK);

    let (status, denied) = response_json(
        client
            .post(format!("{}{}", ddb.api_endpoint(), CAPABILITIES))
            .json(&json!({}))
            .send()
            .expect("protected v2 method should answer"),
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(denied["code"], "DDB_ERROR_CODE_UNAUTHENTICATED");
}

#[test]
fn cors_is_exact_and_compressed_requests_fail_before_decoding() {
    let ddb = DdbProcess::spawn_with_v2_conf(
        &[],
        "  api_cors_allowed_origins:\n    - \"https://debug.example\"",
    );
    let client = Client::new();

    let allowed = client
        .post(format!("{}{}", ddb.api_endpoint(), CAPABILITIES))
        .bearer_auth(V2_TEST_READ_TOKEN)
        .header(ORIGIN, "https://debug.example")
        .json(&json!({}))
        .send()
        .expect("allowed browser origin should answer");
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        allowed.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
        "https://debug.example"
    );

    let denied = client
        .post(format!("{}{}", ddb.api_endpoint(), CAPABILITIES))
        .bearer_auth(V2_TEST_READ_TOKEN)
        .header(ORIGIN, "https://evil.example")
        .json(&json!({}))
        .send()
        .expect("denied browser origin should receive a typed error");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(denied.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    let denied: Value = denied.json().expect("origin denial should be JSON");
    assert_eq!(denied["code"], "DDB_ERROR_CODE_PERMISSION_DENIED");

    let (status, compressed) = response_json(
        client
            .post(format!("{}{}", ddb.api_endpoint(), SERVER_INFO))
            .header(CONTENT_ENCODING, "gzip")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .expect("compressed request should receive a typed error"),
    );
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(compressed["code"], "DDB_ERROR_CODE_UNSUPPORTED");
}

#[test]
fn request_rate_limit_is_enforced_at_the_process_boundary() {
    let ddb = DdbProcess::spawn_with_v2_conf(&[], "  api_requests_per_second: 1");
    let client = Client::new();
    // Startup probes may consume the initial token. Wait for one full refill
    // interval so the assertions do not depend on harness timing.
    thread::sleep(Duration::from_millis(1_100));

    let first = client
        .post(format!("{}{}", ddb.api_endpoint(), SERVER_INFO))
        .json(&json!({}))
        .send()
        .expect("first request should answer");
    assert_eq!(first.status(), StatusCode::OK);

    let second = client
        .post(format!("{}{}", ddb.api_endpoint(), SERVER_INFO))
        .json(&json!({}))
        .send()
        .expect("rate-limited request should answer");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(second.headers().get(RETRY_AFTER).unwrap(), "1");
    let body: Value = second.json().expect("rate limit should be JSON");
    assert_eq!(body["code"], "DDB_ERROR_CODE_RESOURCE_EXHAUSTED");
}

#[test]
fn graceful_shutdown_closes_active_event_streams_before_waiting_for_http_drain() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[]);
    let client = Client::new();
    let stream = client
        .post(format!("{}{}", ddb.api_endpoint(), STATE_EVENTS))
        .bearer_auth(V2_TEST_READ_TOKEN)
        .json(&json!({}))
        .send()
        .expect("state stream should connect");
    assert_eq!(stream.status(), StatusCode::OK);

    let shutdown = client
        .post(format!("{}{}", ddb.api_endpoint(), SHUTDOWN))
        .bearer_auth(V2_TEST_ADMIN_TOKEN)
        .json(&json!({
            "context": {"idempotencyKey": "security-test-shutdown"},
            "target": {"broadcast": {}}
        }))
        .send()
        .expect("shutdown request should answer");
    assert_eq!(shutdown.status(), StatusCode::OK);

    let status = ddb.wait_for_exit();
    assert!(
        status.success(),
        "DDB should drain and exit cleanly: {status}"
    );
    drop(stream);
}
