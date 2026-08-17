mod support;

use std::{
    io::{BufRead, BufReader},
    thread,
    time::{Duration, Instant},
};

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{
    DdbProcess, SessionSpec, V2_TEST_ADMIN_TOKEN, V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN,
};

const RPC_ROOT: &str = "/api/v2/rpc";
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v2",
        alias: "frontend",
        hash: "api-v2-group",
        pid: 702,
        start_delay_ms: 0,
        source_file: "tests/api_v2.rs",
        source_line: 20,
        function: "mock_session",
        exit_on_continue: false,
    }
}

fn rpc(service: &str, method: &str) -> String {
    format!("{RPC_ROOT}/ddb.api.v2.{service}/{method}")
}

fn proto_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn wait_for_terminal_operation(ddb: &DdbProcess, operation_id: &str, token: &str) -> Value {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let (status, response) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "GetOperation"),
            &json!({"operationId": operation_id}),
            token,
        );
        assert_eq!(status, StatusCode::OK, "{response:?}");
        match response["operation"]["state"].as_str() {
            Some("OPERATION_STATE_COMPLETED")
            | Some("OPERATION_STATE_FAILED")
            | Some("OPERATION_STATE_CANCELLED") => return response["operation"].clone(),
            _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            state => panic!("operation {operation_id} did not become terminal: {state:?}"),
        }
    }
}

fn wait_for_frames(ddb: &DdbProcess, thread_id: &str, token: &str) -> Value {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let (status, response) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ListFrames"),
            &json!({"threadId": thread_id, "page": {"pageSize": 20}}),
            token,
        );
        match status {
            StatusCode::OK => return response,
            StatusCode::PRECONDITION_FAILED if Instant::now() < deadline => {
                assert_eq!(response["code"], "DDB_ERROR_CODE_FAILED_PRECONDITION");
                thread::sleep(Duration::from_millis(10));
            }
            _ => panic!("thread did not become readable before timeout: {status} {response:?}"),
        }
    }
}

#[test]
fn distributed_backtrace_frames_are_ordinary_inspectable_frames() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let session_id = sessions["sessions"][0]["sessionId"]
        .as_str()
        .expect("session id should be present");

    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": {"session": {"sessionId": session_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{threads:?}");
    let thread_id = threads["threads"][0]["threadId"]
        .as_str()
        .expect("thread id should be present");

    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "RunDistributedBacktrace"),
        &json!({
            "context": {"idempotencyKey": "v2-dbt-inspectable-frame"},
            "target": {"thread": {"threadId": thread_id}},
            "maxFrames": 32
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("distributed backtrace should be admitted");
    let completed = wait_for_terminal_operation(&ddb, operation_id, V2_TEST_READ_TOKEN);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{completed:?}"
    );
    let frame_id = completed["result"]["distributedBacktrace"]["frames"][0]["frame"]["frameId"]
        .as_str()
        .expect("distributed frame should have an inspectable frame id");

    let (status, scopes) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListScopes"),
        &json!({"frameId": frame_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "distributed frames must compose with ordinary inspection APIs: {scopes:?}"
    );
    assert!(scopes["scopes"]
        .as_array()
        .is_some_and(|scopes| !scopes.is_empty()));
}

#[test]
fn capabilities_report_effective_configured_resource_limits() {
    let ddb = DdbProcess::spawn_with_v2_conf(
        &[],
        "  ApiLimits:\n    state_replay_events: 37\n    state_replay_bytes: 65536\n    state_replay_retention_millis: 1234\n    state_subscriber_queue: 7\n    output_subscriber_queue: 9\n    max_subscribers: 3\n    operation_records: 11\n    operation_bytes: 131072\n    operation_record_bytes: 8192\n    operation_retention_millis: 4321\n    output_event_bytes: 4096",
    );

    let (status, response) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetCapabilities"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{response:?}");
    let limits = &response["capabilities"]["limits"];
    assert_eq!(proto_u64(&limits["maxStateReplayEvents"]), Some(37));
    assert_eq!(proto_u64(&limits["maxStateReplayBytes"]), Some(65_536));
    assert_eq!(
        proto_u64(&limits["stateReplayRetentionMillis"]),
        Some(1_234)
    );
    assert_eq!(limits["stateSubscriberQueue"], 7);
    assert_eq!(limits["outputSubscriberQueue"], 9);
    assert_eq!(limits["maxSubscribers"], 3);
    assert_eq!(limits["maxOperationRecords"], 11);
    assert_eq!(proto_u64(&limits["maxOperationBytes"]), Some(131_072));
    assert_eq!(proto_u64(&limits["maxOperationRecordBytes"]), Some(8_192));
    assert_eq!(proto_u64(&limits["operationRetentionMillis"]), Some(4_321));
    assert_eq!(proto_u64(&limits["maxOutputEventBytes"]), Some(4_096));
    assert_eq!(proto_u64(&limits["maxSourceBytes"]), Some(2_097_152));
}

#[test]
fn v2_protojson_is_fail_closed_scope_checked_and_idempotent_end_to_end() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let (status, server_info) =
        ddb.api_post_json(&rpc("DebuggerService", "GetServerInfo"), &json!({}));
    assert_eq!(status, StatusCode::OK, "{server_info:?}");
    assert_eq!(server_info["serverInfo"]["name"], "ddb");

    let (status, health) = ddb.api_post_json(&rpc("DdbAdminService", "GetHealth"), &json!({}));
    assert_eq!(status, StatusCode::OK, "{health:?}");
    assert!(health["health"]["components"].is_array());

    let (status, unauthenticated) =
        ddb.api_post_json(&rpc("DebuggerService", "GetCapabilities"), &json!({}));
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthenticated["code"], "DDB_ERROR_CODE_UNAUTHENTICATED");

    let (status, invalid_token) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetCapabilities"),
        &json!({}),
        "invalid-invalid-invalid-invalid-invalid-token",
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(invalid_token["code"], "DDB_ERROR_CODE_UNAUTHENTICATED");

    let (status, capabilities) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetCapabilities"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{capabilities:?}");
    assert_eq!(capabilities["capabilities"]["authenticationMode"], "bearer");
    assert!(capabilities["capabilities"]["supportedOperations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "OPERATION_KIND_EXECUTE"));
    assert!(capabilities["capabilities"]["supportedResources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "RESOURCE_KIND_PROCESS"));
    assert!(capabilities["capabilities"]["supportedResources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "RESOURCE_KIND_PENDING_COMMAND"));
    assert!(capabilities["capabilities"]["outputStreamKinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "OUTPUT_STREAM_KIND_CONSOLE"));

    let (status, unauthenticated_output) = ddb.api_post_json(
        &rpc("DdbEventService", "SubscribeOutput"),
        &json!({
            "filter": {"threadIds": ["thr_invalid"]}
        }),
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthenticated_output["code"],
        "DDB_ERROR_CODE_UNAUTHENTICATED"
    );
    let (status, unsupported_output_filter) = ddb.api_post_json_with_bearer(
        &rpc("DdbEventService", "SubscribeOutput"),
        &json!({"filter": {"threadIds": ["thr_invalid"]}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        unsupported_output_filter["code"],
        "DDB_ERROR_CODE_UNSUPPORTED"
    );
    assert_eq!(
        unsupported_output_filter["requiredCapability"],
        "output.thread_context"
    );

    let (status, pending) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListPendingCommands"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{pending:?}");
    for command in pending["pendingCommands"].as_array().into_iter().flatten() {
        assert!(command["pendingCommandId"]
            .as_str()
            .is_some_and(|id| id.starts_with("cmd_")));
        assert!(command["sessionId"].as_str().is_some());
        assert!(command.get("command").is_none());
    }

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let session_id = sessions["sessions"][0]["sessionId"]
        .as_str()
        .expect("session id should be an opaque string")
        .to_string();
    let (status, processes) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListProcesses"),
        &json!({"target": {"session": {"sessionId": session_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{processes:?}");
    assert_eq!(processes["processes"].as_array().map(Vec::len), Some(1));
    let process_id = processes["processes"][0]["processId"]
        .as_str()
        .expect("process id should be an opaque string")
        .to_string();
    assert!(process_id.starts_with("prc_"));
    assert_eq!(processes["processes"][0]["systemProcessId"], "702");
    let (status, process) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetProcess"),
        &json!({"processId": process_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{process:?}");
    assert_eq!(process["process"]["processId"], process_id);

    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": {"session": {"sessionId": session_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{threads:?}");
    let thread_id = threads["threads"][0]["threadId"]
        .as_str()
        .expect("thread id should be an opaque string")
        .to_string();
    assert!(thread_id.starts_with("thr_"));
    assert_eq!(threads["threads"][0]["processId"], process_id);

    let (status, topology) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetSnapshot"),
        &json!({"sections": ["SNAPSHOT_SECTION_TOPOLOGY"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{topology:?}");
    assert_eq!(
        topology["snapshot"]["processes"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        topology["snapshot"]["threads"].as_array().map(Vec::len),
        Some(1)
    );

    let (status, thread) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetThread"),
        &json!({"threadId": thread_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{thread:?}");
    assert_eq!(thread["thread"]["threadId"], thread_id);

    let (status, execution_before) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetExecutionState"),
        &json!({"target": {"thread": {"threadId": thread_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{execution_before:?}");
    assert!(!execution_before["executionState"]["running"]
        .as_bool()
        .unwrap_or(false));
    let execution_state_id = execution_before["executionState"]["executionStateId"]
        .as_str()
        .expect("execution state should have an opaque identity")
        .to_string();
    assert!(execution_state_id.starts_with("exe_"));
    let execution_revision_before = proto_u64(&execution_before["executionState"]["revision"])
        .expect("execution state should be revisioned");

    let (status, frames_before) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListFrames"),
        &json!({"threadId": thread_id, "page": {"pageSize": 20}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{frames_before:?}");
    let frame_id_before = frames_before["frames"][0]["frameId"]
        .as_str()
        .expect("frame id should be an opaque string")
        .to_string();
    assert!(frame_id_before.starts_with("frm_"));
    assert_eq!(frames_before["frames"][0]["location"]["line"], 20);

    let (status, scopes) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListScopes"),
        &json!({"frameId": frame_id_before}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{scopes:?}");
    let scope_id = scopes["scopes"][0]["scopeId"]
        .as_str()
        .expect("scope id should be opaque")
        .to_string();
    assert!(scope_id.starts_with("scp_"));
    assert_eq!(scopes["scopes"][0]["kind"], "SCOPE_KIND_LOCALS");

    let (status, variables) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListVariables"),
        &json!({"scopeId": scope_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{variables:?}");
    assert_eq!(variables["variables"].as_array().map(Vec::len), Some(2));
    assert_eq!(variables["variables"][0]["name"], "counter");
    assert_eq!(variables["variables"][0]["value"], "42");
    assert_eq!(variables["variables"][1]["name"], "request");
    assert_eq!(variables["variables"][1]["hasChildren"], true);
    let request_variable_id = variables["variables"][1]["variableId"]
        .as_str()
        .expect("expandable variable should have an opaque identity")
        .to_string();

    let (status, variable_page_one) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ExpandVariable"),
        &json!({"variableId": request_variable_id, "page": {"pageSize": 2}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{variable_page_one:?}");
    assert_eq!(
        variable_page_one["variables"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(variable_page_one["variables"][0]["name"], "headers");
    assert_eq!(variable_page_one["variables"][0]["hasChildren"], true);
    assert_eq!(variable_page_one["variables"][1]["name"], "payload");
    let headers_variable_id = variable_page_one["variables"][0]["variableId"]
        .as_str()
        .expect("child variable should have an opaque identity")
        .to_string();
    let variable_page_token = variable_page_one["page"]["nextPageToken"]
        .as_str()
        .expect("first child page should have a continuation token")
        .to_string();

    let (status, nested_variables) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ExpandVariable"),
        &json!({"variableId": headers_variable_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{nested_variables:?}");
    assert_eq!(nested_variables["variables"][0]["name"], "trace_id");
    assert_eq!(nested_variables["variables"][0]["value"], "abc123");
    assert_eq!(nested_variables["variables"][1]["name"], "span_id");

    let (status, variable_page_two) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ExpandVariable"),
        &json!({
            "variableId": request_variable_id,
            "page": {"pageSize": 2, "pageToken": variable_page_token}
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{variable_page_two:?}");
    assert_eq!(
        variable_page_two["variables"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(variable_page_two["variables"][0]["name"], "flags");
    assert!(variable_page_two["page"]["nextPageToken"].is_null());

    let (status, register_page_one) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListRegisters"),
        &json!({
            "frameId": frame_id_before,
            "format": "REGISTER_FORMAT_HEXADECIMAL",
            "page": {"pageSize": 2}
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{register_page_one:?}");
    assert_eq!(
        register_page_one["registers"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(register_page_one["registers"][0]["name"], "rax");
    assert_eq!(register_page_one["registers"][0]["value"], "42");
    assert_eq!(register_page_one["registers"][0]["formattedValue"], "0x2a");
    let register_page_token = register_page_one["page"]["nextPageToken"]
        .as_str()
        .expect("first register page should have a continuation token")
        .to_string();
    let (status, register_page_two) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListRegisters"),
        &json!({
            "frameId": frame_id_before,
            "format": "REGISTER_FORMAT_HEXADECIMAL",
            "page": {"pageSize": 2, "pageToken": register_page_token}
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{register_page_two:?}");
    assert_eq!(
        register_page_two["registers"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(register_page_two["registers"][0]["name"], "rip");
    assert!(register_page_two["page"]["nextPageToken"].is_null());

    let thread_target = json!({"thread": {"threadId": thread_id}});
    let (status, denied_memory) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ReadMemory"),
        &json!({"target": thread_target, "address": "0x1000", "byteCount": "8"}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied_memory:?}");
    assert_eq!(denied_memory["code"], "DDB_ERROR_CODE_PERMISSION_DENIED");

    let (status, memory) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ReadMemory"),
        &json!({"target": thread_target, "address": "0x1000", "byteCount": "8"}),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{memory:?}");
    assert_eq!(memory["memory"]["address"], "0x1000");
    assert_eq!(memory["memory"]["data"], "KgAAAAAAAAA=");
    assert!(memory["memory"]["unreadableBytes"].is_null());

    let (status, invalid_memory) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ReadMemory"),
        &json!({"target": thread_target, "address": "0x1000", "byteCount": "0"}),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid_memory:?}");
    assert_eq!(invalid_memory["code"], "DDB_ERROR_CODE_INVALID_ARGUMENT");

    let (status, evaluation) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Evaluate"),
        &json!({
            "context": {"idempotencyKey": "frame-evaluation-before-step"},
            "target": thread_target,
            "expression": "counter",
            "frameId": frame_id_before,
            "evaluationContext": "EVALUATION_CONTEXT_WATCH"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{evaluation:?}");
    let evaluation_id = evaluation["operation"]["operationId"]
        .as_str()
        .expect("evaluation should be admitted");
    let evaluation = wait_for_terminal_operation(&ddb, evaluation_id, V2_TEST_READ_TOKEN);
    assert_eq!(evaluation["state"], "OPERATION_STATE_COMPLETED");
    assert_eq!(
        evaluation["result"]["evaluation"]["expression"],
        "<redacted>"
    );
    assert_eq!(evaluation["result"]["evaluation"]["value"], "42");

    let (status, resolved_source) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ResolveSource"),
        &json!({
            "target": {"session": {"sessionId": session_id}},
            "location": frames_before["frames"][0]["location"]
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{resolved_source:?}");
    let source_reference = resolved_source["source"]["sourceReference"]
        .as_str()
        .expect("source reference should be opaque")
        .to_string();
    assert!(source_reference.starts_with("src_"));

    let (status, source) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ReadSource"),
        &json!({
            "sourceReference": source_reference,
            "startLine": 18,
            "maxLines": 6
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{source:?}");
    assert_eq!(source["source"]["startLine"], 18);
    assert_eq!(source["source"]["lineCount"], 6);
    assert!(source["source"]["content"]
        .as_str()
        .is_some_and(|text| !text.is_empty()));
    assert!(source["source"]["source"]["contentHash"]
        .as_str()
        .is_some_and(|hash| hash.starts_with("sha256:")));

    let (status, unknown_source) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ResolveSource"),
        &json!({"location": {"path": "/etc/passwd", "line": 1}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown_source:?}");
    assert_eq!(unknown_source["code"], "DDB_ERROR_CODE_NOT_FOUND");

    let invalid_step = json!({
        "context": {"idempotencyKey": "invalid-session-step"},
        "target": {"session": {"sessionId": session_id}},
        "action": "EXECUTION_ACTION_NEXT"
    });

    let (status, forbidden) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &invalid_step,
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(forbidden["code"], "DDB_ERROR_CODE_PERMISSION_DENIED");

    let (status, forbidden) = ddb.api_post_json_with_bearer(
        &rpc("DdbAdminService", "Shutdown"),
        &json!({}),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(forbidden["code"], "DDB_ERROR_CODE_PERMISSION_DENIED");

    let (status, admin_reached_handler) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &json!({}),
        V2_TEST_ADMIN_TOKEN,
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        admin_reached_handler["code"],
        "DDB_ERROR_CODE_INVALID_ARGUMENT"
    );

    let (status, invalid_target) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &invalid_step,
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_target["code"], "DDB_ERROR_CODE_INVALID_ARGUMENT");

    let execute = json!({
        "context": {"idempotencyKey": "http-v2-next"},
        "target": {"thread": {"threadId": thread_id}},
        "action": "EXECUTION_ACTION_NEXT"
    });

    let events = ddb.api_post_stream_with_bearer(
        &rpc("DdbEventService", "SubscribeStateEvents"),
        &json!({
            "filter": {
                "kinds": ["STATE_EVENT_KIND_OPERATION_CHANGED"],
                "resourceKinds": ["RESOURCE_KIND_OPERATION"]
            }
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(
        events.headers()[reqwest::header::CONTENT_TYPE],
        "application/x-ndjson"
    );

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &execute,
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation_id = admitted["operation"]["operationId"]
        .as_str()
        .expect("admission should return an operation id")
        .to_string();

    let mut events = BufReader::new(events);
    let mut event_line = String::new();
    events
        .read_line(&mut event_line)
        .expect("state event should be readable");
    let event: Value = serde_json::from_str(event_line.trim())
        .expect("state stream should contain one ProtoJSON object per line");
    assert_eq!(event["kind"], "STATE_EVENT_KIND_OPERATION_CHANGED");
    assert_eq!(event["resourceKind"], "RESOURCE_KIND_OPERATION");
    assert_eq!(event["resourceId"], operation_id);
    assert_eq!(event["operationId"], operation_id);
    assert!(event["cursor"]["sequence"].as_str().is_some());
    let accepted_cursor = event["cursor"].clone();
    drop(events);

    let completed = wait_for_terminal_operation(&ddb, &operation_id, V2_TEST_READ_TOKEN);
    assert_eq!(completed["state"], "OPERATION_STATE_COMPLETED");
    assert_eq!(completed["kind"], "OPERATION_KIND_EXECUTE");

    let replay = ddb.api_post_stream_with_bearer(
        &rpc("DdbEventService", "SubscribeStateEvents"),
        &json!({
            "afterCursor": accepted_cursor,
            "filter": {
                "kinds": ["STATE_EVENT_KIND_OPERATION_CHANGED"],
                "resourceKinds": ["RESOURCE_KIND_OPERATION"]
            }
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(replay.status(), StatusCode::OK);
    let mut replay = BufReader::new(replay);
    let mut replay_line = String::new();
    replay
        .read_line(&mut replay_line)
        .expect("replayed state event should be readable");
    let replayed: Value =
        serde_json::from_str(replay_line.trim()).expect("replayed event should be ProtoJSON");
    assert_eq!(replayed["operationId"], operation_id);
    assert!(
        proto_u64(&replayed["cursor"]["sequence"]) > proto_u64(&event["cursor"]["sequence"]),
        "replay must begin strictly after the supplied cursor"
    );

    let frames_after = wait_for_frames(&ddb, &thread_id, V2_TEST_READ_TOKEN);
    assert_eq!(frames_after["frames"][0]["location"]["line"], 21);
    assert_ne!(
        frames_after["frames"][0]["frameId"], frame_id_before,
        "frame identities must expire after execution changes"
    );
    let (status, stale_scope) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListVariables"),
        &json!({"scopeId": scope_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::GONE, "{stale_scope:?}");
    assert_eq!(stale_scope["code"], "DDB_ERROR_CODE_EXPIRED");
    let (status, stale_variable) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ExpandVariable"),
        &json!({"variableId": request_variable_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::GONE, "{stale_variable:?}");
    assert_eq!(stale_variable["code"], "DDB_ERROR_CODE_EXPIRED");
    let (status, stale_registers) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListRegisters"),
        &json!({"frameId": frame_id_before}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::GONE, "{stale_registers:?}");
    assert_eq!(stale_registers["code"], "DDB_ERROR_CODE_EXPIRED");
    let (status, stale_evaluation) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Evaluate"),
        &json!({
            "context": {"idempotencyKey": "stale-frame-evaluation"},
            "target": thread_target,
            "expression": "counter",
            "frameId": frame_id_before,
            "evaluationContext": "EVALUATION_CONTEXT_WATCH"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::GONE, "{stale_evaluation:?}");
    assert_eq!(stale_evaluation["code"], "DDB_ERROR_CODE_EXPIRED");

    let (status, execution_after) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetExecutionState"),
        &json!({"target": {"thread": {"threadId": thread_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{execution_after:?}");
    assert!(!execution_after["executionState"]["running"]
        .as_bool()
        .unwrap_or(false));
    assert_eq!(
        execution_after["executionState"]["executionStateId"],
        execution_state_id
    );
    assert!(
        proto_u64(&execution_after["executionState"]["revision"])
            .is_some_and(|revision| revision > execution_revision_before),
        "execution-state revision should advance after stepping"
    );

    let (status, replay) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &execute,
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{replay:?}");
    assert_eq!(replay["operation"]["operationId"], operation_id);

    let conflicting_execute = json!({
        "context": {"idempotencyKey": "http-v2-next"},
        "target": {"thread": {"threadId": thread_id}},
        "action": "EXECUTION_ACTION_CONTINUE"
    });
    let (status, conflict) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &conflicting_execute,
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "DDB_ERROR_CODE_CONFLICT");
}
