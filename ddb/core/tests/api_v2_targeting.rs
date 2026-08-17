mod support;

use std::{
    collections::HashSet,
    io::{BufRead, BufReader},
    thread,
    time::{Duration, Instant},
};

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{
    session_id_by_tag, DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN,
};

const RPC_ROOT: &str = "/api/v2/rpc";
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

fn rpc(service: &str, method: &str) -> String {
    format!("{RPC_ROOT}/ddb.api.v2.{service}/{method}")
}

fn proto_u64(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .expect("ProtoJSON uint64 should be numeric or decimal text")
}

fn execution_revision(ddb: &DdbProcess, session_id: &str) -> u64 {
    let (status, response) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "GetExecutionState"),
        &json!({"target": {"session": {"sessionId": session_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{response:?}");
    proto_u64(&response["executionState"]["revision"])
}

fn wait_for_terminal_operation(ddb: &DdbProcess, operation_id: &str) -> Value {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let (status, response) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerService", "GetOperation"),
            &json!({"operationId": operation_id}),
            V2_TEST_READ_TOKEN,
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

fn raw_breakpoint_count(ddb: &DdbProcess, session_id: &str, idempotency_key: &str) -> usize {
    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "ExecuteRawCommand"),
        &json!({
            "context": {"idempotencyKey": idempotency_key},
            "target": {"session": {"sessionId": session_id}},
            "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": "-break-list"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("raw inspection should return an operation id"),
    );
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{operation:?}"
    );
    operation["result"]["rawCommand"]["value"]["objectValue"]["fields"]["BreakpointTable"]
        ["objectValue"]["fields"]["body"]["listValue"]["values"]
        .as_array()
        .map_or(0, Vec::len)
}

#[test]
fn group_execution_control_never_broadcasts_outside_the_resolved_target() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[
        SessionSpec {
            tag: "target-a",
            alias: "target-a",
            hash: "target-group",
            pid: 721,
            start_delay_ms: 0,
            source_file: "src/target_a.rs",
            source_line: 11,
            function: "target_a",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "target-b",
            alias: "target-b",
            hash: "target-group",
            pid: 722,
            start_delay_ms: 0,
            source_file: "src/target_b.rs",
            source_line: 22,
            function: "target_b",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "excluded",
            alias: "excluded",
            hash: "excluded-group",
            pid: 723,
            start_delay_ms: 0,
            source_file: "src/excluded.rs",
            source_line: 33,
            function: "excluded",
            exit_on_continue: false,
        },
    ]);
    ddb.wait_for_sessions_len(3);
    ddb.wait_for_stdout_count("thread-created", 3);
    ddb.wait_for_stdout_count("*stopped", 3);

    let (status, groups) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListGroups"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{groups:?}");
    let target_group = groups["groups"]
        .as_array()
        .expect("groups should be an array")
        .iter()
        .find(|group| group["sessionIds"].as_array().map(Vec::len) == Some(2))
        .expect("one group should contain the two intended sessions");
    let group_id = target_group["groupId"]
        .as_str()
        .expect("group should have an opaque id")
        .to_string();
    let targeted = target_group["sessionIds"]
        .as_array()
        .expect("group should list sessions")
        .iter()
        .map(|id| id.as_str().expect("session id should be text").to_string())
        .collect::<HashSet<_>>();

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let all_sessions = sessions["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .map(|session| {
            session["sessionId"]
                .as_str()
                .expect("session should have an opaque id")
                .to_string()
        })
        .collect::<HashSet<_>>();
    let excluded = all_sessions
        .difference(&targeted)
        .next()
        .expect("one session should be outside the target group")
        .clone();

    let revisions_before = all_sessions
        .iter()
        .map(|session_id| (session_id.clone(), execution_revision(&ddb, session_id)))
        .collect::<Vec<_>>();

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &json!({
            "context": {"idempotencyKey": "group-target-isolation"},
            "target": {"group": {"groupId": group_id}},
            "action": "EXECUTION_ACTION_CONTINUE"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation_id = admitted["operation"]["operationId"]
        .as_str()
        .expect("admission should return an operation id");
    let operation = wait_for_terminal_operation(&ddb, operation_id);
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{operation:?}"
    );

    let outcome_sessions = operation["targetOutcomes"]
        .as_array()
        .expect("fanout operation should retain target outcomes")
        .iter()
        .map(|outcome| {
            outcome["target"]["session"]["sessionId"]
                .as_str()
                .expect("each concrete outcome should identify its session")
                .to_string()
        })
        .collect::<HashSet<_>>();
    assert_eq!(outcome_sessions, targeted);

    for (session_id, revision_before) in revisions_before {
        let revision_after = execution_revision(&ddb, &session_id);
        if targeted.contains(&session_id) {
            assert!(
                revision_after > revision_before,
                "targeted session {session_id} did not observe execution"
            );
        } else {
            assert_eq!(session_id, excluded);
            assert_eq!(
                revision_after, revision_before,
                "session outside the target group was executed"
            );
        }
    }
}

#[test]
fn fanout_operation_reports_partial_backend_rejection_without_state_drift() {
    let mut ddb = DdbProcess::spawn_with_v2_auth_rejection(
        &[
            SessionSpec {
                tag: "successful",
                alias: "successful",
                hash: "partial-group",
                pid: 731,
                start_delay_ms: 0,
                source_file: "src/successful.rs",
                source_line: 41,
                function: "successful",
                exit_on_continue: false,
            },
            SessionSpec {
                tag: "rejecting",
                alias: "rejecting",
                hash: "partial-group",
                pid: 732,
                start_delay_ms: 0,
                source_file: "src/rejecting.rs",
                source_line: 42,
                function: "rejecting",
                exit_on_continue: false,
            },
        ],
        "rejecting",
        "-record-time-and-continue",
    );
    ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);
    ddb.wait_for_stdout_count("*stopped", 2);

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    let sessions = sessions["sessions"]
        .as_array()
        .expect("sessions should be an array");
    let session_id = |name: &str| {
        sessions
            .iter()
            .find(|session| {
                session["displayName"]
                    .as_str()
                    .is_some_and(|display_name| display_name.starts_with(name))
            })
            .and_then(|session| session["sessionId"].as_str())
            .expect("named session should have an opaque id")
            .to_string()
    };
    let successful_id = session_id("successful");
    let rejecting_id = session_id("rejecting");
    let successful_revision_before = execution_revision(&ddb, &successful_id);
    let rejecting_revision_before = execution_revision(&ddb, &rejecting_id);

    let (status, groups) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListGroups"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{groups:?}");
    let group_id = groups["groups"][0]["groupId"]
        .as_str()
        .expect("group should have an opaque id");
    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &json!({
            "context": {"idempotencyKey": "partial-backend-rejection"},
            "target": {"group": {"groupId": group_id}},
            "action": "EXECUTION_ACTION_CONTINUE"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation_id = admitted["operation"]["operationId"]
        .as_str()
        .expect("admission should return an operation id");
    let operation = wait_for_terminal_operation(&ddb, operation_id);

    assert_eq!(
        operation["state"], "OPERATION_STATE_FAILED",
        "{operation:?}"
    );
    assert_eq!(
        operation["error"]["code"], "DDB_ERROR_CODE_PARTIAL_FAILURE",
        "{operation:?}"
    );
    assert_eq!(
        operation["error"]["targetFailures"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert!(operation["result"]["noContent"].is_object());
    assert!(
        !serde_json::to_string(&operation)
            .unwrap()
            .contains("configured mock command rejection"),
        "backend diagnostics must not cross the stable API boundary"
    );

    let outcomes = operation["targetOutcomes"]
        .as_array()
        .expect("fanout operation should retain concrete outcomes");
    assert_eq!(outcomes.len(), 2);
    let outcome_for = |session_id: &str| {
        outcomes
            .iter()
            .find(|outcome| outcome["target"]["session"]["sessionId"].as_str() == Some(session_id))
            .expect("session should have a target outcome")
    };
    assert_eq!(outcome_for(&successful_id)["succeeded"], true);
    assert!(!outcome_for(&rejecting_id)["succeeded"]
        .as_bool()
        .unwrap_or(false));
    assert_eq!(
        outcome_for(&rejecting_id)["error"]["code"],
        "DDB_ERROR_CODE_BACKEND_FAILED"
    );

    assert!(execution_revision(&ddb, &successful_id) > successful_revision_before);
    assert_eq!(
        execution_revision(&ddb, &rejecting_id),
        rejecting_revision_before,
        "rejected target must not be projected as running"
    );
}

#[test]
fn disabled_breakpoint_creation_is_atomic_and_backend_visible() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[SessionSpec {
        tag: "disabled-create",
        alias: "disabled-create",
        hash: "disabled-create-group",
        pid: 732,
        start_delay_ms: 0,
        source_file: "src/disabled_create.rs",
        source_line: 45,
        function: "disabled_create",
        exit_on_continue: false,
    }]);
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
        .expect("session should have an opaque id");

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "CreateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "disabled-breakpoint-create"},
            "target": {"session": {"sessionId": session_id}},
            "breakpoint": {
                "source": {"source": "src/disabled_create.rs", "line": 45},
                "enabled": false
            }
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("create should return an operation id"),
    );
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{operation:?}"
    );
    assert!(
        !operation["result"]["breakpoint"]["spec"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "{operation:?}"
    );

    assert_eq!(
        raw_breakpoint_count(&ddb, session_id, "disabled-breakpoint-inspect"),
        1
    );
    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "ExecuteRawCommand"),
        &json!({
            "context": {"idempotencyKey": "disabled-breakpoint-state"},
            "target": {"session": {"sessionId": session_id}},
            "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": "-break-list"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("inspection should return an operation id"),
    );
    let enabled = &operation["result"]["rawCommand"]["value"]["objectValue"]["fields"]
        ["BreakpointTable"]["objectValue"]["fields"]["body"]["listValue"]["values"][0]
        ["objectValue"]["fields"]["bkpt"]["objectValue"]["fields"]["enabled"]["stringValue"];
    assert_eq!(enabled, "n", "{operation:?}");
}

#[test]
fn omitted_breakpoint_enabled_state_uses_the_conventional_enabled_default() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[SessionSpec {
        tag: "default-enabled-create",
        alias: "default-enabled-create",
        hash: "default-enabled-create-group",
        pid: 733,
        start_delay_ms: 0,
        source_file: "src/default_enabled_create.rs",
        source_line: 46,
        function: "default_enabled_create",
        exit_on_continue: false,
    }]);
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
        .expect("session should have an opaque id");

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "CreateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "default-enabled-breakpoint-create"},
            "target": {"session": {"sessionId": session_id}},
            "breakpoint": {
                "source": {"source": "src/default_enabled_create.rs", "line": 46}
            }
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("create should return an operation id"),
    );
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{operation:?}"
    );
    assert_eq!(
        operation["result"]["breakpoint"]["spec"]["enabled"], true,
        "{operation:?}"
    );
    let breakpoint_id = operation["result"]["breakpoint"]["breakpointId"]
        .as_str()
        .expect("created breakpoint should have an opaque id");

    let (status, rejected) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "UpdateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "missing-enabled-update-value"},
            "breakpointId": breakpoint_id,
            "target": {"session": {"sessionId": session_id}},
            "breakpoint": {},
            "updateMask": "enabled"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected:?}");
    assert_eq!(
        rejected["code"], "DDB_ERROR_CODE_INVALID_ARGUMENT",
        "{rejected:?}"
    );

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "ExecuteRawCommand"),
        &json!({
            "context": {"idempotencyKey": "default-enabled-breakpoint-state"},
            "target": {"session": {"sessionId": session_id}},
            "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
            "command": "-break-list"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("inspection should return an operation id"),
    );
    let enabled = &operation["result"]["rawCommand"]["value"]["objectValue"]["fields"]
        ["BreakpointTable"]["objectValue"]["fields"]["body"]["listValue"]["values"][0]
        ["objectValue"]["fields"]["bkpt"]["objectValue"]["fields"]["enabled"]["stringValue"];
    assert_eq!(enabled, "y", "{operation:?}");
}

#[test]
fn distributed_breakpoint_creation_retains_and_reports_partial_success() {
    let mut ddb = DdbProcess::spawn_with_v2_auth_rejection(
        &[
            SessionSpec {
                tag: "breakpoint-successful",
                alias: "breakpoint-successful",
                hash: "breakpoint-atomic-group",
                pid: 733,
                start_delay_ms: 0,
                source_file: "src/atomic_breakpoint.rs",
                source_line: 47,
                function: "breakpoint_successful",
                exit_on_continue: false,
            },
            SessionSpec {
                tag: "breakpoint-rejecting",
                alias: "breakpoint-rejecting",
                hash: "breakpoint-atomic-group",
                pid: 734,
                start_delay_ms: 0,
                source_file: "src/atomic_breakpoint.rs",
                source_line: 47,
                function: "breakpoint_rejecting",
                exit_on_continue: false,
            },
        ],
        "breakpoint-rejecting",
        "-break-insert",
    );
    ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);
    ddb.wait_for_stdout_count("*stopped", 2);

    let (status, groups) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListGroups"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{groups:?}");
    let group_id = groups["groups"][0]["groupId"]
        .as_str()
        .expect("group should have an opaque id");
    let group_target = json!({"group": {"groupId": group_id}});

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "CreateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "atomic-breakpoint-create"},
            "target": group_target,
            "breakpoint": {
                "source": {"source": "src/atomic_breakpoint.rs", "line": 47},
                "enabled": true
            }
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("create should return an operation id"),
    );
    assert_eq!(
        operation["state"], "OPERATION_STATE_FAILED",
        "{operation:?}"
    );
    assert_eq!(
        operation["error"]["code"], "DDB_ERROR_CODE_PARTIAL_FAILURE",
        "{operation:?}"
    );
    let outcomes = operation["targetOutcomes"]
        .as_array()
        .expect("partial creation should retain concrete outcomes");
    assert_eq!(outcomes.len(), 2, "{operation:?}");
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome["succeeded"] == true)
            .count(),
        1,
        "{operation:?}"
    );
    let created_breakpoint_id = operation["result"]["breakpoint"]["breakpointId"]
        .as_str()
        .expect("partial creation should return the manageable logical breakpoint");

    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": group_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{breakpoints:?}");
    assert_eq!(
        breakpoints["breakpoints"].as_array().map_or(0, Vec::len),
        1,
        "the successful local must remain represented by a logical breakpoint"
    );
    assert_eq!(
        breakpoints["breakpoints"][0]["breakpointId"],
        created_breakpoint_id
    );

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    for (index, session) in sessions["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .enumerate()
    {
        let session_id = session["sessionId"]
            .as_str()
            .expect("session should have an opaque id");
        let display_name = session["displayName"]
            .as_str()
            .expect("session should have a display name");
        let expected_count = usize::from(display_name.starts_with("breakpoint-successful"));
        assert_eq!(
            raw_breakpoint_count(
                &ddb,
                session_id,
                &format!("atomic-breakpoint-inspect-{index}")
            ),
            expected_count,
            "debugger-local breakpoint state disagrees with the reported target outcome for {session_id}"
        );
    }
}

#[test]
fn distributed_breakpoint_deletion_retains_and_reports_failed_locals() {
    let mut ddb = DdbProcess::spawn_with_v2_auth_rejection(
        &[
            SessionSpec {
                tag: "delete-successful",
                alias: "delete-successful",
                hash: "breakpoint-delete-group",
                pid: 737,
                start_delay_ms: 0,
                source_file: "src/delete_breakpoint.rs",
                source_line: 49,
                function: "delete_successful",
                exit_on_continue: false,
            },
            SessionSpec {
                tag: "delete-rejecting",
                alias: "delete-rejecting",
                hash: "breakpoint-delete-group",
                pid: 738,
                start_delay_ms: 0,
                source_file: "src/delete_breakpoint.rs",
                source_line: 49,
                function: "delete_rejecting",
                exit_on_continue: false,
            },
        ],
        "delete-rejecting",
        "-break-delete",
    );
    ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);
    ddb.wait_for_stdout_count("*stopped", 2);

    let (status, groups) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListGroups"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{groups:?}");
    let group_id = groups["groups"][0]["groupId"]
        .as_str()
        .expect("group should have an opaque id");
    let group_target = json!({"group": {"groupId": group_id}});
    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "CreateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "partial-delete-create"},
            "target": group_target,
            "breakpoint": {
                "source": {"source": "src/delete_breakpoint.rs", "line": 49},
                "enabled": true
            }
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let created = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("create should return an operation id"),
    );
    assert_eq!(created["state"], "OPERATION_STATE_COMPLETED", "{created:?}");
    let breakpoint_id = created["result"]["breakpoint"]["breakpointId"]
        .as_str()
        .expect("created breakpoint should have an opaque id");

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "DeleteBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "partial-breakpoint-delete"},
            "target": group_target,
            "breakpointId": breakpoint_id
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let deleted = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("delete should return an operation id"),
    );
    assert_eq!(deleted["state"], "OPERATION_STATE_FAILED", "{deleted:?}");
    assert_eq!(
        deleted["error"]["code"], "DDB_ERROR_CODE_PARTIAL_FAILURE",
        "{deleted:?}"
    );
    assert_eq!(
        deleted["result"]["breakpoint"]["breakpointId"], breakpoint_id,
        "{deleted:?}"
    );
    assert_eq!(
        deleted["targetOutcomes"]
            .as_array()
            .expect("partial deletion should retain concrete outcomes")
            .iter()
            .filter(|outcome| outcome["succeeded"] == true)
            .count(),
        1,
        "{deleted:?}"
    );

    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": group_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{breakpoints:?}");
    assert_eq!(
        breakpoints["breakpoints"].as_array().map_or(0, Vec::len),
        1,
        "the undeleted local must remain represented by a logical breakpoint"
    );

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    for (index, session) in sessions["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .enumerate()
    {
        let session_id = session["sessionId"]
            .as_str()
            .expect("session should have an opaque id");
        let display_name = session["displayName"]
            .as_str()
            .expect("session should have a display name");
        let expected_count = usize::from(display_name.starts_with("delete-rejecting"));
        assert_eq!(
            raw_breakpoint_count(&ddb, session_id, &format!("partial-delete-inspect-{index}")),
            expected_count,
            "debugger-local deletion state disagrees with the target outcome for {session_id}"
        );
    }
}

#[test]
fn combined_breakpoint_update_rolls_back_every_debugger_target_on_failure() {
    let mut ddb = DdbProcess::spawn_with_v2_auth_rejection(
        &[
            SessionSpec {
                tag: "rollback-successful",
                alias: "rollback-successful",
                hash: "rollback-group",
                pid: 735,
                start_delay_ms: 0,
                source_file: "src/rollback.rs",
                source_line: 61,
                function: "rollback_successful",
                exit_on_continue: false,
            },
            SessionSpec {
                tag: "rollback-rejecting",
                alias: "rollback-rejecting",
                hash: "rollback-group",
                pid: 736,
                start_delay_ms: 100,
                source_file: "src/rollback.rs",
                source_line: 61,
                function: "rollback_rejecting",
                exit_on_continue: false,
            },
        ],
        "rollback-rejecting",
        "-break-condition",
    );
    ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);
    ddb.wait_for_stdout_count("*stopped", 2);

    let legacy_sessions = ddb.api_get("/sessions");
    assert!(
        session_id_by_tag(&legacy_sessions, "rollback-successful")
            < session_id_by_tag(&legacy_sessions, "rollback-rejecting"),
        "the successful target must be visited before the injected failure"
    );

    let (status, groups) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListGroups"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{groups:?}");
    let group_id = groups["groups"][0]["groupId"]
        .as_str()
        .expect("group should have an opaque id");
    let group_target = json!({"group": {"groupId": group_id}});

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "CreateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "rollback-create"},
            "target": group_target,
            "breakpoint": {
                "source": {"source": "src/rollback.rs", "line": 61},
                "enabled": true
            }
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("create should return an operation id"),
    );
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{operation:?}"
    );
    let breakpoint_id = operation["result"]["breakpoint"]["breakpointId"]
        .as_str()
        .expect("created breakpoint should have an opaque id");

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "UpdateBreakpoint"),
        &json!({
            "context": {"idempotencyKey": "rollback-combined-update"},
            "breakpointId": breakpoint_id,
            "target": group_target,
            "breakpoint": {"enabled": false, "condition": "request.id == 42"},
            "updateMask": "enabled,condition"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation = wait_for_terminal_operation(
        &ddb,
        admitted["operation"]["operationId"]
            .as_str()
            .expect("update should return an operation id"),
    );
    assert_eq!(
        operation["state"], "OPERATION_STATE_FAILED",
        "{operation:?}"
    );
    assert_eq!(
        operation["error"]["code"], "DDB_ERROR_CODE_BACKEND_FAILED",
        "{operation:?}"
    );

    let (status, breakpoints) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListBreakpoints"),
        &json!({"target": group_target}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{breakpoints:?}");
    assert_eq!(breakpoints["breakpoints"][0]["spec"]["enabled"], true);
    assert!(breakpoints["breakpoints"][0]["spec"]["condition"].is_null());

    let (status, sessions) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListSessions"),
        &json!({}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{sessions:?}");
    for (index, session) in sessions["sessions"]
        .as_array()
        .expect("sessions should be an array")
        .iter()
        .enumerate()
    {
        let session_id = session["sessionId"]
            .as_str()
            .expect("session should have an opaque id");
        let (status, admitted) = ddb.api_post_json_with_bearer(
            &rpc("DebuggerControlService", "ExecuteRawCommand"),
            &json!({
                "context": {"idempotencyKey": format!("rollback-inspect-{index}")},
                "target": {"session": {"sessionId": session_id}},
                "dialect": "RAW_COMMAND_DIALECT_GDB_MI",
                "command": "-break-list"
            }),
            V2_TEST_CONTROL_TOKEN,
        );
        assert_eq!(status, StatusCode::OK, "{admitted:?}");
        let operation = wait_for_terminal_operation(
            &ddb,
            admitted["operation"]["operationId"]
                .as_str()
                .expect("inspection should return an operation id"),
        );
        assert_eq!(
            operation["state"], "OPERATION_STATE_COMPLETED",
            "{operation:?}"
        );
        let fields = &operation["result"]["rawCommand"]["value"]["objectValue"]["fields"]
            ["BreakpointTable"]["objectValue"]["fields"]["body"]["listValue"]["values"][0]
            ["objectValue"]["fields"]["bkpt"]["objectValue"]["fields"];
        assert_eq!(fields["enabled"]["stringValue"], "y", "{operation:?}");
        assert!(fields["cond"].is_null(), "{operation:?}");
    }
}

#[test]
fn pending_command_events_form_a_correlated_revisioned_lifecycle() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[SessionSpec {
        tag: "pending-events",
        alias: "pending-events",
        hash: "pending-events-group",
        pid: 741,
        start_delay_ms: 0,
        source_file: "src/pending_events.rs",
        source_line: 51,
        function: "pending_events",
        exit_on_continue: false,
    }]);
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
        .expect("session should have an opaque id");
    let (status, threads) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerService", "ListThreads"),
        &json!({"target": {"session": {"sessionId": session_id}}}),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{threads:?}");
    let thread_id = threads["threads"][0]["threadId"]
        .as_str()
        .expect("thread should have an opaque id");
    assert_eq!(threads["threads"][0]["sessionId"], session_id);

    let events = ddb.api_post_stream_with_bearer(
        &rpc("DdbEventService", "SubscribeStateEvents"),
        &json!({
            "filter": {
                "resourceKinds": ["RESOURCE_KIND_PENDING_COMMAND"],
                "sessionIds": [session_id]
            }
        }),
        V2_TEST_READ_TOKEN,
    );
    assert_eq!(events.status(), StatusCode::OK);

    let (status, admitted) = ddb.api_post_json_with_bearer(
        &rpc("DebuggerControlService", "Execute"),
        &json!({
            "context": {"idempotencyKey": "pending-command-event-lifecycle"},
            "target": {"thread": {"threadId": thread_id}},
            "action": "EXECUTION_ACTION_NEXT"
        }),
        V2_TEST_CONTROL_TOKEN,
    );
    assert_eq!(status, StatusCode::OK, "{admitted:?}");
    let operation_id = admitted["operation"]["operationId"]
        .as_str()
        .expect("admission should return an operation id");

    let mut events = BufReader::new(events);
    let lifecycle = (0..3)
        .map(|_| {
            let mut line = String::new();
            events
                .read_line(&mut line)
                .expect("pending-command event should be readable");
            assert!(
                !line.is_empty(),
                "state stream ended before lifecycle completion"
            );
            serde_json::from_str::<Value>(line.trim())
                .expect("state stream should contain ProtoJSON")
        })
        .collect::<Vec<_>>();

    let queued = &lifecycle[0];
    let running = &lifecycle[1];
    let removed = &lifecycle[2];
    for event in &lifecycle {
        assert_eq!(event["resourceKind"], "RESOURCE_KIND_PENDING_COMMAND");
        assert_eq!(event["operationId"], operation_id);
    }
    assert_eq!(queued["kind"], "STATE_EVENT_KIND_RESOURCE_UPSERTED");
    assert_eq!(running["kind"], "STATE_EVENT_KIND_RESOURCE_UPSERTED");
    assert_eq!(removed["kind"], "STATE_EVENT_KIND_RESOURCE_DELETED");

    let queued_command = &queued["upsert"]["pendingCommand"];
    let running_command = &running["upsert"]["pendingCommand"];
    let pending_command_id = queued_command["pendingCommandId"]
        .as_str()
        .expect("pending command should have an opaque id");
    assert_eq!(running_command["pendingCommandId"], pending_command_id);
    assert_eq!(removed["resourceId"], pending_command_id);
    assert_eq!(removed["deleted"]["resourceId"], pending_command_id);
    assert_eq!(queued_command["sessionId"], session_id);
    assert_eq!(queued_command["operationId"], operation_id);
    assert_eq!(running_command["operationId"], operation_id);
    assert!(!queued_command["running"].as_bool().unwrap_or(false));
    assert_eq!(running_command["running"], true);

    let queued_revision = proto_u64(&queued["resourceRevision"]);
    let running_revision = proto_u64(&running["resourceRevision"]);
    let removed_revision = proto_u64(&removed["resourceRevision"]);
    assert_eq!(proto_u64(&queued_command["revision"]), queued_revision);
    assert_eq!(proto_u64(&running_command["revision"]), running_revision);
    assert!(running_revision > queued_revision);
    assert!(removed_revision > running_revision);
    assert_eq!(
        proto_u64(&removed["deleted"]["resourceRevision"]),
        removed_revision
    );

    let operation = wait_for_terminal_operation(&ddb, operation_id);
    assert_eq!(
        operation["state"], "OPERATION_STATE_COMPLETED",
        "{operation:?}"
    );
}
