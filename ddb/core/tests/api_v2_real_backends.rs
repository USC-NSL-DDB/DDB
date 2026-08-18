mod support;

use std::{thread, time::Duration};

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{
    build_real_loop_example, real_test_guard, session_id_by_tag, BinarySessionSpec, DdbProcess,
    V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN,
};

const RPC_ROOT: &str = "/api/v2/rpc";

fn rpc(service: &str, method: &str) -> String {
    format!("{RPC_ROOT}/ddb.api.v2.{service}/{method}")
}

fn wait_for_operation(ddb: &DdbProcess, operation_id: &str) -> Value {
    for _ in 0..500 {
        let (status, response) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "GetOperation"),
            &json!({"operationId": operation_id}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{response:?}");
        match response["operation"]["state"].as_str() {
            Some("OPERATION_STATE_COMPLETED") | Some("OPERATION_STATE_FAILED") => {
                return response["operation"].clone()
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("operation {operation_id} did not complete")
}

fn assert_typed_inspection_on_backend(backend: &str) {
    let example = build_real_loop_example();
    let binary_path = example.binary_path.to_string_lossy();
    let source_path = example.source_path.to_string_lossy();
    let tag = format!("v2-real-{backend}");
    let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(
        backend,
        &[BinarySessionSpec {
            tag: &tag,
            alias: &tag,
            hash: "v2-real-backend",
            pid: if backend == "gdb" { 9_301 } else { 9_302 },
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path: &binary_path,
            binary_args: vec![
                "--sleep-ms".to_string(),
                "5".to_string(),
                "--max-iterations".to_string(),
                "100000".to_string(),
            ],
            stop_at_entry: true,
        }],
    );
    let legacy_sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let sid = session_id_by_tag(&legacy_sessions, &tag);
    ddb.send_cmd(&format!(
        "801-break-insert --session {sid} {}:{}",
        source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("801^done");
    ddb.send_cmd(&format!("802-exec-continue --session {sid}"));
    ddb.wait_for_stdout_line("802^running");
    ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        &format!("session-id=\"{sid}\""),
    ]);

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {sessions:?}");
    let session_id = sessions["sessions"][0]["sessionId"]
        .as_str()
        .expect("session id should be present");
    let session_target = json!({"session": {"sessionId": session_id}});
    let (status, capabilities) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetCapabilities"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {capabilities:?}");
    let breakpoint_features = capabilities["capabilities"]["breakpointFeatures"]
        .as_array()
        .expect("breakpoint features should be present");
    for required in [
        "BREAKPOINT_FEATURE_SOURCE",
        "BREAKPOINT_FEATURE_CONDITION",
        "BREAKPOINT_FEATURE_TEMPORARY",
        "BREAKPOINT_FEATURE_ENABLE_DISABLE",
    ] {
        assert!(
            breakpoint_features
                .iter()
                .any(|feature| feature == required),
            "{backend}: missing {required}: {capabilities:?}"
        );
    }
    assert_eq!(
        breakpoint_features
            .iter()
            .any(|feature| feature == "BREAKPOINT_FEATURE_HARDWARE"),
        backend == "gdb",
        "{backend}: {capabilities:?}"
    );

    if backend == "lldb" {
        let (status, unsupported) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "CreateBreakpoint"),
            &json!({
                "context": {"idempotencyKey": "v2-real-lldb-hardware"},
                "target": session_target,
                "breakpoint": {
                    "source": {
                        "source": source_path.as_ref(),
                        "line": example.breakpoint_line
                    },
                    "enabled": true,
                    "hardware": true
                }
            }),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{unsupported:?}");
        assert_eq!(
            unsupported["code"], "DDB_ERROR_CODE_UNSUPPORTED",
            "{unsupported:?}"
        );
        assert_eq!(
            unsupported["requiredCapability"], "breakpoints.hardware",
            "{unsupported:?}"
        );
    }
    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {threads:?}");
    let thread_id = threads["threads"]
        .as_array()
        .and_then(|threads| {
            threads
                .iter()
                .find(|thread| thread["state"] == "THREAD_STATE_STOPPED")
        })
        .and_then(|thread| thread["threadId"].as_str())
        .expect("a stopped thread should be present");
    let target = json!({"thread": {"threadId": thread_id}});

    let (status, frames) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListFrames"),
        &json!({"threadId": thread_id, "page": {"pageSize": 20}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {frames:?}");
    let frame = frames["frames"]
        .as_array()
        .and_then(|frames| {
            frames.iter().find(|frame| {
                frame["functionName"]
                    .as_str()
                    .is_some_and(|name| name.contains("breakpoint_target"))
            })
        })
        .expect("breakpoint frame should be present");
    let frame_id = frame["frameId"]
        .as_str()
        .expect("frame id should be present");

    let (status, registers) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListRegisters"),
        &json!({
            "frameId": frame_id,
            "format": "REGISTER_FORMAT_HEXADECIMAL",
            "page": {"pageSize": 5}
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {registers:?}");
    assert!(registers["registers"]
        .as_array()
        .is_some_and(|registers| !registers.is_empty()));

    let (status, scopes) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListScopes"),
        &json!({"frameId": frame_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {scopes:?}");
    let scope_id = scopes["scopes"][0]["scopeId"]
        .as_str()
        .expect("scope id should be present");
    let (status, variables) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListVariables"),
        &json!({"scopeId": scope_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {variables:?}");
    let request_variable = variables["variables"]
        .as_array()
        .and_then(|variables| {
            variables
                .iter()
                .find(|variable| variable["name"] == "request")
        })
        .expect("request aggregate should be visible");
    let request_variable_id = request_variable["variableId"]
        .as_str()
        .expect("variable id should be present");
    let (status, children) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ExpandVariable"),
        &json!({"variableId": request_variable_id, "page": {"pageSize": 10}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {children:?}");
    assert!(children["variables"]
        .as_array()
        .is_some_and(|children| children.len() >= 2));

    let (status, memory) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ReadMemory"),
        &json!({"target": target, "address": "&request", "byteCount": "16"}),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {memory:?}");
    assert!(memory["memory"]["data"]
        .as_str()
        .is_some_and(|encoded| !encoded.is_empty()));

    let (status, evaluation) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Evaluate"),
        &json!({
            "context": {"idempotencyKey": format!("v2-real-eval-{backend}")},
            "target": target,
            "expression": "counter",
            "frameId": frame_id,
            "evaluationContext": "EVALUATION_CONTEXT_WATCH"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {evaluation:?}");
    let operation_id = evaluation["operation"]["operationId"]
        .as_str()
        .expect("evaluation should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );
    assert!(completed["result"]["evaluation"]["value"].is_string());

    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {breakpoints:?}");
    let breakpoint_id = breakpoints["breakpoints"][0]["breakpointId"]
        .as_str()
        .expect("breakpoint id should be present");

    for (enabled, suffix) in [(false, "disable"), (true, "enable")] {
        let (status, admission) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "UpdateBreakpoint"),
            &json!({
                "context": {
                    "idempotencyKey": format!("v2-real-{backend}-{suffix}")
                },
                "breakpointId": breakpoint_id,
                "target": session_target,
                "breakpoint": {"enabled": enabled},
                "updateMask": "enabled"
            }),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
        let operation_id = admission["operation"]["operationId"]
            .as_str()
            .expect("breakpoint update should be admitted");
        let completed = wait_for_operation(&ddb, operation_id);
        assert_eq!(
            completed["state"], "OPERATION_STATE_COMPLETED",
            "{backend}: {completed:?}"
        );
    }

    let condition = "counter >= 0";
    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "UpdateBreakpoint"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-condition")
            },
            "breakpointId": breakpoint_id,
            "target": session_target,
            "breakpoint": {"condition": condition},
            "updateMask": "condition"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("condition update should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );

    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {breakpoints:?}");
    assert_eq!(
        breakpoints["breakpoints"][0]["spec"]["condition"], condition,
        "{backend}: {breakpoints:?}"
    );

    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "UpdateBreakpoint"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-clear-condition")
            },
            "breakpointId": breakpoint_id,
            "target": session_target,
            "breakpoint": {},
            "updateMask": "condition"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("condition clear should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );
    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {breakpoints:?}");
    assert!(
        breakpoints["breakpoints"][0]["spec"]["condition"].is_null(),
        "{backend}: {breakpoints:?}"
    );

    let combined_condition = "counter >= 1";
    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "UpdateBreakpoint"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-combined-set")
            },
            "breakpointId": breakpoint_id,
            "target": session_target,
            "breakpoint": {
                "enabled": false,
                "condition": combined_condition
            },
            "updateMask": "enabled,condition"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("combined breakpoint update should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );
    assert!(
        !completed["result"]["breakpoint"]["spec"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "{backend}: {completed:?}"
    );
    assert_eq!(
        completed["result"]["breakpoint"]["spec"]["condition"], combined_condition,
        "{backend}: {completed:?}"
    );

    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "UpdateBreakpoint"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-combined-reset")
            },
            "breakpointId": breakpoint_id,
            "target": session_target,
            "breakpoint": {"enabled": true},
            "updateMask": "enabled,condition"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("combined breakpoint reset should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );
    assert_eq!(
        completed["result"]["breakpoint"]["spec"]["enabled"], true,
        "{backend}: {completed:?}"
    );
    assert!(
        completed["result"]["breakpoint"]["spec"]["condition"].is_null(),
        "{backend}: {completed:?}"
    );

    let create_condition = "counter >= 0";
    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "CreateBreakpoint"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-advanced-create")
            },
            "target": session_target,
            "breakpoint": {
                "source": {
                    "source": source_path.as_ref(),
                    "line": example.breakpoint_line
                },
                "enabled": false,
                "condition": create_condition,
                "temporary": true
            }
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("advanced breakpoint creation should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );
    assert_eq!(
        completed["result"]["breakpoint"]["spec"]["condition"], create_condition,
        "{backend}: {completed:?}"
    );
    assert_eq!(
        completed["result"]["breakpoint"]["spec"]["temporary"], true,
        "{backend}: {completed:?}"
    );
    assert!(
        !completed["result"]["breakpoint"]["spec"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "{backend}: {completed:?}"
    );
    let created_breakpoint_id = completed["result"]["breakpoint"]["breakpointId"]
        .as_str()
        .expect("created breakpoint id should be present");
    let (status, raw_admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "ExecuteRawCommand"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-advanced-inspect")
            },
            "target": session_target,
            "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": "-break-list"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {raw_admission:?}");
    let raw_operation_id = raw_admission["operation"]["operationId"]
        .as_str()
        .expect("raw breakpoint inspection should be admitted");
    let raw_completed = wait_for_operation(&ddb, raw_operation_id);
    assert_eq!(
        raw_completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {raw_completed:?}"
    );
    let local_breakpoints = raw_completed["result"]["rawCommand"]["value"]["objectValue"]["fields"]
        ["BreakpointTable"]["objectValue"]["fields"]["body"]["listValue"]["values"]
        .as_array()
        .expect("break-list should return a body");
    assert!(
        local_breakpoints.iter().any(|entry| {
            let row = &entry["objectValue"]["fields"];
            let fields = if row["bkpt"]["objectValue"]["fields"].is_object() {
                &row["bkpt"]["objectValue"]["fields"]
            } else {
                row
            };
            fields["cond"]["stringValue"] == create_condition
                && fields["enabled"]["stringValue"] == "n"
        }),
        "{backend}: disabled conditional breakpoint was not retained by the backend: {raw_completed:?}"
    );
    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "DeleteBreakpoint"),
        &json!({
            "context": {
                "idempotencyKey": format!("v2-real-{backend}-advanced-delete")
            },
            "breakpointId": created_breakpoint_id,
            "target": session_target
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{backend}: {admission:?}");
    let operation_id = admission["operation"]["operationId"]
        .as_str()
        .expect("advanced breakpoint deletion should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{backend}: {completed:?}"
    );
}

#[test]
fn gdb_serves_typed_v2_inspection_contract() {
    let _guard = real_test_guard();
    assert_typed_inspection_on_backend("gdb");
}

#[test]
fn lldb_serves_typed_v2_inspection_contract() {
    let _guard = real_test_guard();
    assert_typed_inspection_on_backend("lldb");
}

#[test]
fn gdb_raw_breakpoint_command_validates_scope_and_completes_on_a_session() {
    let _guard = real_test_guard();
    let example = build_real_loop_example();
    let binary_path = example.binary_path.to_string_lossy();
    let source_path = example.source_path.to_string_lossy();
    let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(
        "gdb",
        &[BinarySessionSpec {
            tag: "v2-real-raw-gdb",
            alias: "v2-real-raw-gdb",
            hash: "v2-real-raw-gdb",
            pid: 9_303,
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path: &binary_path,
            binary_args: vec![
                "--sleep-ms".to_string(),
                "5".to_string(),
                "--max-iterations".to_string(),
                "100000".to_string(),
            ],
            stop_at_entry: true,
        }],
    );
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
    let session_target = json!({"session": {"sessionId": session_id}});
    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{threads:?}");
    let thread_id = threads["threads"][0]["threadId"]
        .as_str()
        .expect("thread id should be present");
    let command = format!("-break-insert {source_path}:{}", example.breakpoint_line);

    let (status, rejected) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "ExecuteRawCommand"),
        &json!({
            "context": {"idempotencyKey": "v2-real-raw-gdb-thread"},
            "target": {"thread": {"threadId": thread_id}},
            "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": command,
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected:?}");
    assert_eq!(rejected["code"], "DDB_ERROR_CODE_INVALID_ARGUMENT");

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "ExecuteRawCommand"),
        &json!({
            "context": {"idempotencyKey": "v2-real-raw-gdb-session"},
            "target": session_target,
            "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": command,
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation_id = admitted["operation"]["operationId"]
        .as_str()
        .expect("raw command should be admitted");
    let completed = wait_for_operation(&ddb, operation_id);
    assert_eq!(
        completed["state"], "OPERATION_STATE_COMPLETED",
        "{completed:?}"
    );

    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": session_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{breakpoints:?}");
    assert_eq!(breakpoints["breakpoints"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        breakpoints["breakpoints"][0]["spec"]["source"]["line"].as_u64(),
        Some(example.breakpoint_line)
    );
}
