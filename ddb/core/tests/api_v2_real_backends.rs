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

fn completed_control(ddb: &DdbProcess, method: &str, request: Value) -> Value {
    let (status, admission) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", method),
        &request,
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admission:?}");
    let operation =
        wait_for_operation(ddb, admission["operation"]["operationId"].as_str().unwrap());
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{method} {}: {operation:?}",
        request["context"]["idempotencyKey"]
    );
    operation
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

    if backend == "gdb" {
        let caller = frames["frames"]
            .as_array()
            .unwrap()
            .iter()
            .find(|frame| {
                frame["functionName"]
                    .as_str()
                    .is_some_and(|name| name == "ddb_real_loop::main")
            })
            .expect("caller frame should be present");
        assert!(caller["level"].as_u64().unwrap() > 0);
        let (status, admitted) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "ExecuteRawCommand"),
            &json!({
                "context": {"idempotencyKey": "native-console-set"},
                "target": session_target,
                "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": "set $ddb_native_console = sleep_ms + 36",
                "frameId": caller["frameId"]
            }),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{admitted:?}");
        let operation =
            wait_for_operation(&ddb, admitted["operation"]["operationId"].as_str().unwrap());
        assert_eq!(
            operation["state"], "OPERATION_STATE_COMPLETED",
            "{operation:?}"
        );
        let (status, admitted) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "Evaluate"),
            &json!({
                "context": {"idempotencyKey": "native-console-read"},
                "target": session_target,
                "expression": "$ddb_native_console + 1",
                "evaluationContext": "EVALUATION_CONTEXT_WATCH"
            }),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{admitted:?}");
        let operation =
            wait_for_operation(&ddb, admitted["operation"]["operationId"].as_str().unwrap());
        assert_eq!(
            operation["result"]["evaluation"]["value"], "42",
            "{operation:?}"
        );
    }
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
    if backend == "gdb" {
        let filter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/api_frame_filter.py");
        completed_control(
            &ddb,
            "ExecuteRawCommand",
            json!({
                "context": {"idempotencyKey": "install-inspection-frame-filter"},
                "target": session_target, "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": format!("python import runpy; runpy.run_path({})", serde_json::to_string(&filter.to_string_lossy()).unwrap())
            }),
        );
        completed_control(
            &ddb,
            "ExecuteRawCommand",
            json!({
                "context": {"idempotencyKey": "enable-inspection-frame-filter"},
                "target": session_target, "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
                "command": "-enable-frame-filters"
            }),
        );
    }
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
    assert_eq!(
        request_variable["hasChildren"], true,
        "{request_variable:?}"
    );
    assert!(
        request_variable["typeName"]
            .as_str()
            .is_some_and(|name| name.contains("DebugRequest")),
        "{request_variable:?}"
    );
    let counter = variables["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["name"] == "counter")
        .unwrap();
    assert!(
        !counter["hasChildren"].as_bool().unwrap_or(false),
        "{counter:?}"
    );
    assert_eq!(counter["childCount"], "0", "{counter:?}");
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

    let headers = children["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|child| child["name"] == "headers")
        .unwrap();
    let (status, entries) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ExpandVariable"),
        &json!({"variableId": headers["variableId"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{entries:?}");
    let entry = &entries["variables"][0];
    assert!(
        entry["evaluateName"].is_null(),
        "test must exercise a child without an expression"
    );
    let assignment_request = json!({
        "context": {"idempotencyKey": "assign-nested-local"},
        "target": target,
        "variableId": entry["variableId"],
        "value": "123"
    });
    let (status, forbidden) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "SetVariable"),
        &assignment_request,
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::FORBIDDEN, "{forbidden:?}");
    let assigned = completed_control(&ddb, "SetVariable", assignment_request.clone());
    assert_eq!(
        assigned["result"]["variableAssignment"]["value"], "123",
        "{assigned:?}"
    );
    let replay = completed_control(&ddb, "SetVariable", assignment_request);
    assert_eq!(replay["operationId"], assigned["operationId"]);
    let observed = completed_control(
        &ddb,
        "Evaluate",
        json!({
            "context": {"idempotencyKey": "verify-nested-local"},
            "target": target, "frameId": frame_id,
            "expression": "request.headers[0]", "evaluationContext": "EVALUATION_CONTEXT_REPL"
        }),
    );
    assert_eq!(
        observed["result"]["evaluation"]["value"], "123",
        "{observed:?}"
    );

    if backend == "gdb" {
        for request in [
            json!({"context": {"idempotencyKey": "thread-setting"}, "target": target, "enablePrettyPrinting": {}}),
            json!({"context": {"idempotencyKey": "invalid-source-setting"}, "target": session_target,
                "sourceMapping": {"from": "/source\nshow version", "to": "/local"}}),
        ] {
            let (status, rejected) = ddb.api_post_json_with_bearer(
                &rpc("DebuggerControlService", "ConfigureDebugger"),
                &request,
                V2_TEST_CONTROL_TOKEN,
            );
            assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected:?}");
        }
        completed_control(
            &ddb,
            "ConfigureDebugger",
            json!({
                "context": {"idempotencyKey": "typed-pretty-printers"},
                "target": session_target, "enablePrettyPrinting": {}
            }),
        );
        completed_control(
            &ddb,
            "ConfigureDebugger",
            json!({
                "context": {"idempotencyKey": "typed-source-mapping"},
                "target": session_target,
                "sourceMapping": {"from": "/ddb build/source", "to": "/ddb local/source"}
            }),
        );
        completed_control(
            &ddb,
            "ExecuteRawCommand",
            json!({
                "context": {"idempotencyKey": "verify-source-mapping"},
                "target": session_target, "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": "python mapping = gdb.execute('show substitute-path', to_string=True); assert '/ddb build/source' in mapping and '/ddb local/source' in mapping"
            }),
        );
        let printer = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/api_value_printer.py");
        completed_control(
            &ddb,
            "ExecuteRawCommand",
            json!({
                "context": {"idempotencyKey": "install-test-printer"},
                "target": session_target, "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": format!("python import runpy; runpy.run_path({})", serde_json::to_string(&printer.to_string_lossy()).unwrap())
            }),
        );
        let (status, pretty) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ListVariables"),
            &json!({"scopeId": scope_id}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{pretty:?}");
        let request = pretty["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["name"] == "request")
            .unwrap();
        assert_eq!(request["value"], "DDB test request", "{pretty:?}");
        assert_eq!(request["hasChildren"], true);
        assert!(
            request["childCount"].is_null(),
            "dynamic count is not exact"
        );
        let (status, first) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ExpandVariable"),
            &json!({"variableId": request["variableId"], "page": {"pageSize": 1}}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{first:?}");
        assert_eq!(first["variables"].as_array().unwrap().len(), 1);
        let headers = &first["variables"][0];
        assert_eq!(headers["hasChildren"], true, "{headers:?}");
        assert_eq!(headers["presentationHint"], "array", "{headers:?}");
        assert!(headers["childCount"].is_null(), "{headers:?}");
        let (status, second) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ExpandVariable"),
            &json!({"variableId": request["variableId"], "page": {"pageSize": 1, "pageToken": first["page"]["nextPageToken"]}}), V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{second:?}");
        assert_eq!(second["variables"][0]["name"], "flags");
        let (status, nested) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ExpandVariable"),
            &json!({"variableId": headers["variableId"]}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{nested:?}");
        assert_eq!(nested["variables"].as_array().unwrap().len(), 2);
        let assigned = completed_control(
            &ddb,
            "SetVariable",
            json!({
                "context": {"idempotencyKey": "assign-pretty-child"}, "target": target,
                "variableId": nested["variables"][0]["variableId"], "value": "124"
            }),
        );
        assert_eq!(
            assigned["result"]["variableAssignment"]["value"], "124",
            "{assigned:?}"
        );
    }

    // Retain the original evaluation. Expanding it must not execute it again.
    if backend == "gdb" {
        completed_control(
            &ddb,
            "ExecuteRawCommand",
            json!({
                "context": {"idempotencyKey": "retained-eval-setup"},
                "target": target,
                "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": "python gdb.set_convenience_variable('ddb_eval_count', 0); gdb.execute('set language c')"
            }),
        );
    }
    let retained = completed_control(
        &ddb,
        "Evaluate",
        json!({
            "context": {"idempotencyKey": "retained-eval"},
            "target": target,
            "expression": if backend == "gdb" { "($ddb_eval_count += 1, request)" } else { "request" },
            "frameId": frame_id,
            "evaluationContext": "EVALUATION_CONTEXT_WATCH"
        }),
    );
    let retained_id = retained["result"]["evaluation"]["variableId"]
        .as_str()
        .unwrap();
    assert_eq!(
        retained["result"]["evaluation"]["hasChildren"], true,
        "{retained:?}"
    );
    let mut retained_child = None;
    for _ in 0..2 {
        let (status, expanded) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ExpandVariable"),
            &json!({"variableId": retained_id}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{expanded:?}");
        retained_child = expanded["variables"]
            .as_array()
            .unwrap()
            .iter()
            .find(|child| child["name"] == "flags")
            .map(|child| child["variableId"].clone());
        assert!(
            expanded["variables"]
                .as_array()
                .is_some_and(|items| items.len() >= 2),
            "{expanded:?}"
        );
    }
    let assigned = completed_control(
        &ddb,
        "SetVariable",
        json!({
            "context": {"idempotencyKey": "assign-retained-child"},
            "target": target, "variableId": retained_child.unwrap(), "value": "7"
        }),
    );
    assert_eq!(
        assigned["result"]["variableAssignment"]["value"], "7",
        "{assigned:?}"
    );
    let observed = completed_control(
        &ddb,
        "Evaluate",
        json!({
            "context": {"idempotencyKey": "verify-retained-child"},
            "target": target, "frameId": frame_id,
            "expression": "request.flags", "evaluationContext": "EVALUATION_CONTEXT_REPL"
        }),
    );
    assert_eq!(
        observed["result"]["evaluation"]["value"], "7",
        "{observed:?}"
    );
    if backend == "gdb" {
        let count = completed_control(
            &ddb,
            "Evaluate",
            json!({
                "context": {"idempotencyKey": "retained-eval-count"},
                "target": target,
                "expression": "$ddb_eval_count",
                "frameId": frame_id,
                "evaluationContext": "EVALUATION_CONTEXT_REPL"
            }),
        );
        assert_eq!(
            count["result"]["evaluation"]["value"], "1",
            "evaluation ran more than once: {count:?}"
        );
        completed_control(
            &ddb,
            "ExecuteRawCommand",
            json!({
                "context": {"idempotencyKey": "retained-eval-language"},
                "target": target,
                "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": "set language auto"
            }),
        );
    }

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
    if backend == "gdb" {
        // Scalar watches have no expandable identity. Repeated refreshes at the
        // same stop must not consume the shared 1,024-root inspection budget.
        for index in 0..1_025 {
            let evaluated = completed_control(
                &ddb,
                "Evaluate",
                json!({
                    "context": {"idempotencyKey": format!("scalar-refresh-{index}")},
                    "target": target,
                    "expression": "counter",
                    "frameId": frame_id,
                    "evaluationContext": "EVALUATION_CONTEXT_WATCH"
                }),
            );
            assert!(evaluated["result"]["evaluation"]["value"].is_string());
            assert!(evaluated["result"]["evaluation"]["variableId"].is_null());
        }
    }

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
    if backend == "gdb" {
        let (status, admitted) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "ExecuteRawCommand"),
            &json!({
                "context": {"idempotencyKey": "native-console-resume"},
                "target": target,
                "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
                "command": "next",
                "frameId": frame_id
            }),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{admitted:?}");
        let operation =
            wait_for_operation(&ddb, admitted["operation"]["operationId"].as_str().unwrap());
        assert_eq!(
            operation["state"], "OPERATION_STATE_COMPLETED",
            "console execution may invalidate its input frame: {operation:?}"
        );
    }
    if backend == "gdb" {
        // Command completion acknowledges the step; the next stop arrives later.
        let mut stopped = false;
        for _ in 0..500 {
            let (status, threads) = ddb.api_post_json_with_bearer(
                &rpc("DebuggerService", "ListThreads"),
                &json!({"target": target}),
                V2_TEST_READ_TOKEN,
            );
            assert_eq!(status, StatusCode::OK, "{threads:?}");
            if threads["threads"].as_array().is_some_and(|threads| {
                threads.iter().any(|thread| {
                    thread["threadId"] == thread_id && thread["state"] == "THREAD_STATE_STOPPED"
                })
            }) {
                stopped = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(stopped, "native step did not reach its next stop");
        let (status, expired) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ExpandVariable"),
            &json!({"variableId": retained_id}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::GONE, "{expired:?}");
        let (status, expired) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "SetVariable"),
            &json!({"context": {"idempotencyKey": "assign-expired"}, "target": target,
                "variableId": retained_id, "value": "8"}),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::GONE, "{expired:?}");
    }
}

// Both backends must serve the same typed signal operation without relying on
// a console spelling shared with GDB. Exercise stopped and running targets.
fn assert_typed_signal_on_backend(backend: &str) {
    let example = build_real_loop_example();
    let binary_path = example.binary_path.to_string_lossy();
    for running in [false, true] {
        let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(
            backend,
            &[BinarySessionSpec {
                tag: "signal-target",
                alias: "signal-target",
                hash: "signal-target",
                pid: 9_310,
                ip: "127.0.0.1",
                start_delay_ms: 0,
                binary_path: &binary_path,
                binary_args: vec!["--max-iterations".into(), "100000".into()],
                stop_at_entry: true,
            }],
        );
        ddb.wait_for_sessions_len(1);
        ddb.wait_for_stdout_count("*stopped", 1);
        let (status, sessions) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ListSessions"),
            &json!({}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{sessions:?}");
        let target = json!({"session": {"sessionId": sessions["sessions"][0]["sessionId"]}});
        if running {
            completed_control(
                &ddb,
                "Execute",
                json!({
                    "context": {"idempotencyKey": "signal-resume"},
                    "target": target, "action": "EXECUTION_ACTION_CONTINUE"
                }),
            );
        }
        completed_control(
            &ddb,
            "Execute",
            json!({
                "context": {"idempotencyKey": "signal-kill"},
                "target": target, "action": "EXECUTION_ACTION_SIGNAL", "signalName": "SIGKILL"
            }),
        );
        ddb.wait_for_sessions_len(0);
    }
}

fn assert_continue_with_running_peer(backend: &str) {
    let example = build_real_loop_example();
    let binary_path = example.binary_path.to_string_lossy();
    let specs: Vec<_> = ["server", "client"]
        .into_iter()
        .enumerate()
        .map(|(index, tag)| BinarySessionSpec {
            tag,
            alias: tag,
            hash: tag,
            pid: 9_320 + index as u64,
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path: &binary_path,
            binary_args: vec![
                "--max-iterations".into(),
                "100000".into(),
                "--worker-thread".into(),
            ],
            stop_at_entry: true,
        })
        .collect();
    let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(backend, &specs);
    ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("*stopped", 2);
    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let server = &sessions["sessions"][0]["sessionId"];
    completed_control(
        &ddb,
        "Execute",
        json!({
            "context": {"idempotencyKey": "resume-server"},
            "target": {"session": {"sessionId": server}}, "action": "EXECUTION_ACTION_CONTINUE"
        }),
    );
    if backend == "gdb" {
        // GDB reports new worker threads while the process stays running.
        ddb.wait_for_stdout_count("thread-created", 3);
    }
    let operation = completed_control(
        &ddb,
        "Execute",
        json!({
            "context": {"idempotencyKey": "resume-all"},
            "target": {"broadcast": {}}, "action": "EXECUTION_ACTION_CONTINUE"
        }),
    );
    let outcomes = operation["targetOutcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|outcome| outcome["succeeded"] == true));
    for session in sessions["sessions"].as_array().unwrap() {
        let target = json!({"session": {"sessionId": session["sessionId"]}});
        let (status, threads) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ListThreads"),
            &json!({"target": target}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{threads:?}");
        let threads = threads["threads"].as_array().unwrap();
        assert!(!threads.is_empty());
        assert!(threads
            .iter()
            .all(|thread| thread["state"] == "THREAD_STATE_RUNNING"));
        // Repeated Continue must also succeed for an individual running session.
        completed_control(
            &ddb,
            "Execute",
            json!({
                "context": {"idempotencyKey": format!("repeat-{}", session["sessionId"])},
                "target": target, "action": "EXECUTION_ACTION_CONTINUE"
            }),
        );
    }
    completed_control(
        &ddb,
        "Execute",
        json!({
            "context": {"idempotencyKey": "resume-all-running"},
            "target": {"broadcast": {}}, "action": "EXECUTION_ACTION_CONTINUE"
        }),
    );
}

#[test]
fn lldb_continue_with_running_peer() {
    let _guard = real_test_guard();
    assert_continue_with_running_peer("lldb");
}

#[test]
fn gdb_continue_with_running_peer() {
    let _guard = real_test_guard();
    assert_continue_with_running_peer("gdb");
}

#[test]
fn gdb_typed_signal_handles_stopped_and_running_targets() {
    let _guard = real_test_guard();
    assert_typed_signal_on_backend("gdb");
}

#[test]
fn lldb_typed_signal_handles_stopped_and_running_targets() {
    let _guard = real_test_guard();
    assert_typed_signal_on_backend("lldb");
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

#[test]
fn gdb_inspects_optimized_inline_frames_with_filters_enabled() {
    let _guard = real_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("optimized-frame-args");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/optimized_frame_args.cc");
    let compile = std::process::Command::new("g++")
        .args(["-g", "-O2"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("GCC C++ compiler is required for the optimized DWARF fixture");
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let binary_path = binary.to_string_lossy();
    let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(
        "gdb",
        &[BinarySessionSpec {
            tag: "optimized",
            alias: "optimized",
            hash: "optimized",
            pid: 9303,
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path: &binary_path,
            binary_args: vec![],
            stop_at_entry: true,
        }],
    );
    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let sid = session_id_by_tag(&sessions, "optimized");
    ddb.send_cmd(&format!(
        "901-break-insert --session {sid} {}:4",
        source.display()
    ));
    ddb.wait_for_stdout_line("901^done");
    ddb.send_cmd(&format!("902-exec-continue --session {sid}"));
    ddb.wait_for_stdout_count("*stopped", 2);

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let target = json!({"session": {"sessionId": sessions["sessions"][0]["sessionId"]}});
    completed_control(
        &ddb,
        "ExecuteRawCommand",
        json!({
            "context": {"idempotencyKey": "optimized-fixture-symbol"},
            "target": target, "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
            "command": "python from gdb.FrameDecorator import FrameDecorator; assert any(arg.symbol().name == 'unused' and arg.symbol().addr_class == gdb.SYMBOL_LOC_OPTIMIZED_OUT for arg in FrameDecorator(gdb.newest_frame().older()).frame_args()), 'fixture must have an optimized-out inline argument'"
        }),
    );
    // A pass-through filter alone reproduces the GDB bug. No DDB filtering
    // rules, synthetic variables, or intentionally throwing decorators.
    completed_control(
        &ddb,
        "ExecuteRawCommand",
        json!({
            "context": {"idempotencyKey": "optimized-identity-filter"},
            "target": target, "dialect": "RAW_COMMAND_DIALECT_BACKEND_NATIVE",
            "command": "python gdb.frame_filters.clear(); gdb.frame_filters['identity'] = type('Identity', (), {'enabled': True, 'priority': 100, 'filter': lambda self, frames: frames})()"
        }),
    );
    completed_control(
        &ddb,
        "ExecuteRawCommand",
        json!({
            "context": {"idempotencyKey": "optimized-enable-filter"},
            "target": target, "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": "-enable-frame-filters"
        }),
    );
    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{threads:?}");
    let (status, frames) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListFrames"),
        &json!({"threadId": threads["threads"][0]["threadId"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{frames:?}");
    let frame = frames["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|frame| {
            frame["functionName"]
                .as_str()
                .is_some_and(|name| name.starts_with("worker"))
        })
        .expect("optimized inline worker frame must remain visible");
    let (status, scopes) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListScopes"),
        &json!({"frameId": frame["frameId"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{scopes:?}");
    let (status, variables) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListVariables"),
        &json!({"scopeId": scopes["scopes"][0]["scopeId"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{variables:?}");
    assert!(
        variables["variables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|var| var["name"] == "visible" && var["value"] == "8"),
        "{variables:?}"
    );
}

fn assert_frame_variables_exclude_file_globals(backend: &str) {
    let _guard = real_test_guard();
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("variable-scopes");
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/variable_scopes.cc");
    let line = std::fs::read_to_string(&source)
        .unwrap()
        .lines()
        .position(|line| line.contains("VARIABLES_MARKER"))
        .unwrap()
        + 1;
    let compile = std::process::Command::new("g++")
        .args(["-g", "-O0"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("GCC C++ compiler is required for the scope fixture");
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let binary_path = binary.to_string_lossy();
    let mut ddb = DdbProcess::spawn_real_binary_sessions_with_v2_auth(
        backend,
        &[BinarySessionSpec {
            tag: "variable-scopes",
            alias: "variable-scopes",
            hash: "variable-scopes",
            pid: 9304,
            ip: "127.0.0.1",
            start_delay_ms: 0,
            binary_path: &binary_path,
            binary_args: vec![],
            stop_at_entry: true,
        }],
    );
    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let sid = session_id_by_tag(&sessions, "variable-scopes");
    ddb.send_cmd(&format!(
        "901-break-insert --session {sid} {}:{line}",
        source.display()
    ));
    ddb.wait_for_stdout_line("901^done");
    ddb.send_cmd(&format!("902-exec-continue --session {sid}"));
    ddb.wait_for_stdout_line("902^running");
    ddb.wait_for_stdout_count("*stopped", 2);
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
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let target = json!({"session": {"sessionId": sessions["sessions"][0]["sessionId"]}});
    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{threads:?}");
    let (status, frames) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListFrames"),
        &json!({"threadId": threads["threads"][0]["threadId"]}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{frames:?}");
    let frame_id = &frames["frames"][0]["frameId"];
    let (status, scopes) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListScopes"),
        &json!({"frameId": frame_id}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{scopes:?}");
    for _ in 0..2 {
        let (status, variables) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "ListVariables"),
            &json!({"scopeId": scopes["scopes"][0]["scopeId"]}),
            V2_TEST_READ_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{variables:?}");
        let mut names = variables["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["argument", "local", "local_static", "nested", "nested_static"],
            "{backend} must retain block statics, exclude globals and expired blocks, and not duplicate variables: {variables:?}");
    }
    let evaluated = completed_control(
        &ddb,
        "Evaluate",
        json!({
            "context": {"idempotencyKey": "watch-global"}, "target": target,
            "frameId": frame_id, "expression": "library::global_noise",
            "evaluationContext": "EVALUATION_CONTEXT_WATCH"
        }),
    );
    assert_eq!(
        evaluated["result"]["evaluation"]["value"], "91",
        "{evaluated:?}"
    );
}

#[test]
fn lldb_frame_variables_exclude_file_globals() {
    assert_frame_variables_exclude_file_globals("lldb");
}

#[test]
fn gdb_frame_variables_exclude_file_globals() {
    assert_frame_variables_exclude_file_globals("gdb");
}
