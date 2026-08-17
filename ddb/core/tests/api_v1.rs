mod support;

use reqwest::StatusCode;
use serde_json::json;
use support::{session_id_by_tag, DdbProcess, SessionSpec};

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v1",
        alias: "frontend",
        hash: "api-v1-group",
        pid: 701,
        start_delay_ms: 0,
        source_file: "tests/api_v1.rs",
        source_line: 9,
        function: "mock_session",
        exit_on_continue: false,
    }
}

#[test]
fn versioned_api_hydrates_clients_and_normalizes_debugger_values() {
    let mut ddb = DdbProcess::spawn(&[mock_session()]);
    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    let sid = session_id_by_tag(&sessions, "api-v1");

    let service = ddb.api_get("/api/v1/");
    assert_eq!(service["api_version"], "v1");
    assert_eq!(service["data"]["name"], "ddb");
    assert_eq!(service["data"]["backend"], "mock");

    let capabilities = ddb.api_get("/api/v1/capabilities");
    assert_eq!(
        capabilities["data"]["protocol"]["generic_command_passthrough"],
        true
    );
    assert!(capabilities["data"]["inspection"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item == "stack_variables"));
    assert_eq!(capabilities["data"]["extensions"], json!([]));
    assert!(!capabilities["data"]["ddb_features"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item == "proclet_ownership"));

    let state = ddb.api_get("/api/v1/state");
    assert_eq!(state["data"]["sessions"][0]["sid"], sid);
    assert!(state["data"]["sessions"][0]["all_threads_stopped"].is_boolean());
    assert_eq!(state["data"]["groups"][0]["hash"], "api-v1-group");
    assert_eq!(state["data"]["extensions"], json!([]));
    assert!(state["data"].get("proclet_owners").is_none());

    let (status, threads) = ddb.api_post_json(
        "/api/v1/threads/query",
        &json!({"target": {"kind": "session", "session_id": sid}}),
    );
    assert_eq!(status, StatusCode::OK);
    let thread = &threads["data"]["result"]["responses"][0]["payload"]["threads"][0];
    assert!(thread["id"].is_string());
    assert!(thread["id"].get("String").is_none());
}

#[test]
fn typed_inspection_breakpoint_and_source_endpoints_share_the_command_engine() {
    let mut ddb = DdbProcess::spawn(&[mock_session()]);
    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    let sid = session_id_by_tag(&sessions, "api-v1");

    let (status, threads) = ddb.api_post_json(
        "/api/v1/threads/query",
        &json!({"target": {"kind": "session", "session_id": sid}}),
    );
    assert_eq!(status, StatusCode::OK);
    let thread_id = threads["data"]["result"]["responses"][0]["payload"]["threads"][0]["id"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();

    let (status, frames) = ddb.api_post_json(
        "/api/v1/stack/frames",
        &json!({"thread_id": thread_id, "low": 0, "high": 20}),
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        frames["data"]["result"]["responses"][0]["payload"]["stack"][0]["func"],
        "mock_session"
    );

    let (status, next) = ddb.api_post_json(
        "/api/v1/execution",
        &json!({
            "action": "next",
            "target": {"kind": "thread", "thread_id": thread_id}
        }),
    );
    assert_eq!(status, StatusCode::OK, "{next:?}");
    ddb.wait_for_stdout_line("end-stepping-range");
    let (status, frames) = ddb.api_post_json(
        "/api/v1/stack/frames",
        &json!({"thread_id": thread_id, "low": 0, "high": 20}),
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        frames["data"]["result"]["responses"][0]["payload"]["stack"][0]["line"],
        "10"
    );

    for (action, argument) in [
        ("jump", json!({"location": "tests/api_v1.rs:11"})),
        ("send_signal", json!({"signal": "SIGUSR1"})),
    ] {
        let mut request = json!({
            "action": action,
            "target": {"kind": "thread", "thread_id": thread_id}
        });
        request
            .as_object_mut()
            .expect("execution request should be an object")
            .extend(
                argument
                    .as_object()
                    .expect("execution argument should be an object")
                    .clone(),
            );
        let (status, response) = ddb.api_post_json("/api/v1/execution", &request);
        assert_eq!(status, StatusCode::OK, "{action}: {response:?}");
    }

    let (status, variables) = ddb.api_post_json(
        "/api/v1/stack/variables",
        &json!({"thread_id": thread_id, "frame": 0, "values": "simple"}),
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        variables["data"]["result"]["responses"][0]["payload"]["variables"][0]["name"],
        "counter"
    );

    let (status, memory) = ddb.api_post_json(
        "/api/v1/memory/read",
        &json!({
            "address": "$sp + 16",
            "count": 8,
            "offset": -8,
            "target": {"kind": "thread", "thread_id": thread_id}
        }),
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        memory["data"]["result"]["responses"][0]["payload"]["memory"][0]["contents"],
        "2a00000000000000"
    );

    let (status, breakpoint) = ddb.api_post_json(
        "/api/v1/breakpoints",
        &json!({
            "source": "tests/api_v1.rs",
            "line": 9,
            "target": {"kind": "session", "session_id": sid}
        }),
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        breakpoint["data"]["result"]["responses"][0]["payload"]["bkpt"]["fullname"],
        "tests/api_v1.rs"
    );
    let breakpoints = ddb.api_get("/api/v1/breakpoints");
    let breakpoint_id = breakpoints["data"]["items"][0]["id"].as_u64().unwrap();
    let (status, disabled) = ddb.api_patch_json(
        &format!("/api/v1/breakpoints/{breakpoint_id}"),
        &json!({"enabled": false}),
    );
    assert_eq!(status, StatusCode::OK, "{disabled:?}");
    assert_eq!(
        ddb.api_get("/api/v1/breakpoints")["data"]["items"][0]["enabled"],
        false
    );
    let (status, enabled) = ddb.api_patch_json(
        &format!("/api/v1/breakpoints/{breakpoint_id}"),
        &json!({"enabled": true}),
    );
    assert_eq!(status, StatusCode::OK, "{enabled:?}");
    assert_eq!(
        ddb.api_get("/api/v1/breakpoints")["data"]["items"][0]["enabled"],
        true
    );

    let source =
        ddb.api_get("/api/v1/sources/content?path=tests%2Fapi_v1.rs&start_line=1&end_line=8");
    assert_eq!(source["data"]["start_line"], 1);
    assert_eq!(source["data"]["end_line"], 8);
    assert!(source["data"]["lines"].as_array().unwrap().len() == 8);
}

#[test]
fn command_failures_use_the_versioned_error_contract() {
    let ddb = DdbProcess::spawn(&[]);
    let (status, error) =
        ddb.api_post_json("/api/v1/commands", &json!({"command": "", "wait": true}));

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["api_version"], "v1");
    assert_eq!(error["error"]["code"], "empty_command");
    assert!(error["request_id"].is_string());
}
