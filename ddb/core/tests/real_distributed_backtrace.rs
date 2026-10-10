mod support;

use std::{
    path::Path,
    time::{Duration, Instant},
};

use serde_json::{json, Value};

use support::{
    build_real_dbt_example, capture_session_context, real_test_guard, session_id_by_tag,
    BinarySessionSpec, DdbProcess, V2_TEST_CONTROL_TOKEN,
};
use tempfile::tempdir;

const DBT_IP: &str = "127.0.0.1";
const DBT_GROUP: &str = "grp-real-dbt";

#[derive(Clone, Copy)]
enum DebuggerUnderTest {
    Gdb,
    Lldb,
}

fn logical_pid(role_index: usize) -> u64 {
    9_500 + role_index as u64
}

fn session_tag(role_index: usize) -> String {
    format!("{DBT_IP}:-{}", logical_pid(role_index))
}

fn spawn_real_dbt(
    debugger: DebuggerUnderTest,
    depth: usize,
    binary_path: &str,
    ctx_dir: &Path,
) -> DdbProcess {
    let tags = (1..=depth).map(session_tag).collect::<Vec<_>>();
    let specs = (1..=depth)
        .map(|role_index| {
            let mut binary_args = vec![
                "--logical-pid".to_string(),
                logical_pid(role_index).to_string(),
                "--role-index".to_string(),
                role_index.to_string(),
                "--self-ctx-file".to_string(),
                ctx_dir
                    .join(format!("ctx-{role_index}.txt"))
                    .to_str()
                    .expect("context path should be valid utf-8")
                    .to_string(),
            ];

            if role_index > 1 {
                binary_args.extend([
                    "--parent-ctx-file".to_string(),
                    ctx_dir
                        .join(format!("ctx-{}.txt", role_index - 1))
                        .to_str()
                        .expect("parent context path should be valid utf-8")
                        .to_string(),
                    "--caller-ip".to_string(),
                    DBT_IP.to_string(),
                    "--caller-pid".to_string(),
                    logical_pid(role_index - 1).to_string(),
                    "--caller-tid".to_string(),
                    "1".to_string(),
                ]);
            }

            BinarySessionSpec {
                tag: tags[role_index - 1].as_str(),
                alias: tags[role_index - 1].as_str(),
                hash: DBT_GROUP,
                pid: logical_pid(role_index),
                ip: DBT_IP,
                start_delay_ms: 0,
                binary_path,
                binary_args,
                stop_at_entry: false,
            }
        })
        .collect::<Vec<_>>();

    match debugger {
        DebuggerUnderTest::Gdb => DdbProcess::spawn_real_dbt_sessions(&specs),
        DebuggerUnderTest::Lldb => DdbProcess::spawn_lldb_dbt_sessions(&specs),
    }
}

fn extract_first_thread_id(line: &str) -> u64 {
    let (_, threads) = line.split_once("threads=[").unwrap_or_else(|| {
        panic!("thread-info output should include a threads payload, got: {line}");
    });

    for (offset, _) in threads.match_indices("id=\"") {
        if offset == 0 {
            continue;
        }
        let prefix = threads.as_bytes()[offset - 1];
        if prefix != b'{' && prefix != b',' {
            continue;
        }
        let rest = &threads[offset + 4..];
        let id = rest
            .split('"')
            .next()
            .expect("thread id should terminate with a quote");
        return id.parse::<u64>().unwrap_or_else(|error| {
            panic!("thread id should be a valid integer in `{line}`: {error}");
        });
    }

    panic!("thread-info output should include a thread id, got: {line}");
}

fn resolve_single_thread_gtid(ddb: &mut DdbProcess, sid: u64) -> u64 {
    let token = 8_000 + sid;
    ddb.send_cmd(&format!("{token}-thread-info --session {sid}"));
    let output = ddb.wait_for_stdout_line(&format!("{token}^done"));
    extract_first_thread_id(&output)
}

fn write_context_file(
    ctx_dir: &Path,
    role_index: usize,
    context: &std::collections::BTreeMap<String, u64>,
) {
    let payload = ["pc", "sp", "fp", "lr"]
        .into_iter()
        .filter_map(|register| {
            context
                .get(register)
                .map(|value| format!("{register}={value}\n"))
        })
        .collect::<String>();
    std::fs::write(ctx_dir.join(format!("ctx-{role_index}.txt")), payload)
        .expect("context file should be written");
}

fn assert_public_caller_memory(ddb: &DdbProcess, depth: usize) -> String {
    let rpc = |service: &str, method: &str, body: Value| {
        let (status, response) = ddb.api_post_json_with_bearer(
            &format!("/api/v2/rpc/ddb.api.v2.{service}/{method}"),
            &body,
            V2_TEST_CONTROL_TOKEN,
        );
        assert!(status.is_success(), "{method}: {response}");
        response
    };
    let sessions = rpc("DebuggerService", "ListSessions", json!({}));
    let session = |role| {
        sessions["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["tag"] == session_tag(role))
            .unwrap()["sessionId"]
            .clone()
    };
    let root = session(1);
    let leaf = session(depth);
    assert_ne!(root, leaf);
    let threads = rpc(
        "DebuggerService",
        "ListThreads",
        json!({
            "target": {"session": {"sessionId": leaf}}
        }),
    );
    let origin = threads["threads"][0]["threadId"].clone();
    let admission = rpc(
        "DebuggerControlService",
        "RunDistributedBacktrace",
        json!({
            "context": {"idempotencyKey": "real-caller-memory"},
            "target": {"thread": {"threadId": origin}}, "maxFrames": 256
        }),
    );
    let operation_id = &admission["operation"]["operationId"];
    let deadline = Instant::now() + Duration::from_secs(15);
    let operation = loop {
        let response = rpc(
            "DebuggerService",
            "GetOperation",
            json!({"operationId": operation_id}),
        );
        let operation = response["operation"].clone();
        match operation["state"].as_str() {
            Some("OPERATION_STATE_COMPLETED") => break operation,
            Some("OPERATION_STATE_FAILED" | "OPERATION_STATE_CANCELLED") => panic!("{operation}"),
            _ => assert!(
                Instant::now() < deadline,
                "distributed operation timed out: {operation}"
            ),
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let frame = operation["result"]["distributedBacktrace"]["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|frame| {
            frame["sessionId"] == root
                && frame["frame"]["functionName"]
                    .as_str()
                    .is_some_and(|name| name.contains("root_stop"))
        })
        .expect("distributed root caller frame");
    let owner = &frame["threadId"];
    assert_ne!(
        owner, &origin,
        "caller inspection must not use the leaf thread"
    );
    assert!(frame["frame"]["level"]
        .as_u64()
        .is_some_and(|level| level > 0));
    let scopes = rpc(
        "DebuggerService",
        "ListScopes",
        json!({"frameId": frame["frame"]["frameId"]}),
    );
    let locals = scopes["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["kind"] == "SCOPE_KIND_LOCALS")
        .unwrap();
    let variables = rpc(
        "DebuggerService",
        "ListVariables",
        json!({"scopeId": locals["scopeId"]}),
    );
    let marker = variables["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "memory_marker")
        .unwrap_or_else(|| panic!("caller marker missing: {variables}"));
    let mut children = rpc(
        "DebuggerService",
        "ExpandVariable",
        json!({"variableId": marker["variableId"]}),
    );
    if let Some(access) = children["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "public")
    {
        children = rpc(
            "DebuggerService",
            "ExpandVariable",
            json!({"variableId": access["variableId"]}),
        );
    }
    let value = children["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "value")
        .expect("caller marker field");
    assert_eq!(value["value"], "9501");
    let address = value["address"]
        .as_str()
        .expect("caller field storage address");
    let memory = rpc(
        "DebuggerService",
        "ReadMemory",
        json!({
            "target": {"thread": {"threadId": owner}}, "address": address, "byteCount": "8"
        }),
    );
    let memory: ddb_api_types::v2::ReadMemoryResponse = serde_json::from_value(memory).unwrap();
    assert_eq!(memory.memory.unwrap().data, 9501_u64.to_ne_bytes());
    frame["frame"]["frameId"].as_str().unwrap().to_owned()
}

fn assert_distributed_backtrace(debugger: DebuggerUnderTest, depth: usize) {
    let _guard = real_test_guard();
    let example = build_real_dbt_example();
    let binary_path = example
        .binary_path
        .to_str()
        .expect("fixture binary path should be valid utf-8");
    let ctx_dir = tempdir().expect("temporary context directory should be created");
    let mut ddb = spawn_real_dbt(debugger, depth, binary_path, ctx_dir.path());

    let sessions = ddb.wait_for_sessions_len(depth);
    ddb.wait_for_stdout_count("thread-created", depth);
    let mut caller_pcs = Vec::new();
    for role_index in 1..=depth {
        let sid = session_id_by_tag(&sessions, &session_tag(role_index));
        let sid_needle = format!("session-id=\"{sid}\"");
        ddb.wait_for_stdout_line_with_all(&["*stopped", sid_needle.as_str()]);
        ddb.wait_for_session_stopped(sid);
        let context = capture_session_context(&ddb, sid);
        assert!(
            context.get("pc").is_some_and(|value| *value != 0)
                && context.get("sp").is_some_and(|value| *value != 0),
            "role {role_index} session {sid} produced an incomplete register context: {context:?}"
        );
        write_context_file(ctx_dir.path(), role_index, &context);
        if role_index < depth {
            caller_pcs.push((sid, context["pc"]));
            // The real RPC caller keeps running after capturing its metadata.
            // Reconstruct an earlier stack, not the same stop cached by LLDB.
            let token = 7_000 + sid;
            ddb.send_cmd(&format!("{token}-exec-continue --session {sid}"));
            ddb.wait_for_stdout_line(&format!("{token}^running"));
        }
    }

    for (sid, pc) in &caller_pcs {
        let token = 7_500 + sid;
        ddb.send_cmd(&format!("{token}-exec-interrupt --session {sid}"));
        ddb.wait_for_stdout_line(&format!("{token}^done"));
        ddb.wait_for_session_stopped(*sid);
        assert_ne!(capture_session_context(&ddb, *sid)["pc"], *pc);
        let thread = resolve_single_thread_gtid(&mut ddb, *sid);
        // Populate LLDB's frame cache, as VS Code does on a paused thread.
        let token = 7_600 + sid;
        ddb.send_cmd(&format!("{token}-stack-list-frames --thread {thread}"));
        ddb.wait_for_stdout_line(&format!("{token}^done"));
    }

    let leaf_sid = session_id_by_tag(&sessions, &session_tag(depth));
    let root_sid = session_id_by_tag(&sessions, &session_tag(1));
    let leaf_gtid = resolve_single_thread_gtid(&mut ddb, leaf_sid);

    if depth > 1 {
        let token = 8_500 + depth as u64;
        ddb.send_cmd(&format!("{token}-get-remote-bt --thread {leaf_gtid}"));
        let output = ddb.wait_for_stdout_line(&format!("{token}^done"));
        assert!(
            output.contains("message=\"success\""),
            "remote-bt metadata lookup failed for depth {depth}: {output}"
        );
    }

    let token = 9_000 + depth as u64;
    ddb.send_cmd(&format!("{token}-bt-remote --thread {leaf_gtid}"));
    let output = ddb.wait_for_stdout_line(&format!("{token}^done"));

    assert!(
        output.contains("stack=["),
        "unexpected DBT output: {output}"
    );
    assert!(
        output.contains(&format!("session=\"{leaf_sid}\"")),
        "leaf session {leaf_sid} missing from DBT output: {output}"
    );
    assert!(
        output.contains(&format!("session=\"{root_sid}\"")),
        "root session {root_sid} missing from DBT output: {output}"
    );
    for (_, pc) in &caller_pcs {
        assert!(
            output.split("addr=\"").skip(1).any(|part| {
                part.split('"').next().and_then(|address| {
                    u64::from_str_radix(address.trim_start_matches("0x"), 16).ok()
                }) == Some(*pc)
            }),
            "saved caller PC {pc:#x} missing from reconstructed stack: {output}"
        );
    }
    assert_eq!(
        output.matches("boundary_frame=\"1\"").count(),
        depth.saturating_sub(1),
        "unexpected boundary-frame count for depth {depth}: {output}"
    );

    let caller_frame = (depth > 1).then(|| assert_public_caller_memory(&ddb, depth));

    for (sid, _) in caller_pcs {
        let in_custom_context = |ddb: &DdbProcess| {
            ddb.api_get("/sessions")
                .as_array()
                .unwrap()
                .iter()
                .find(|session| session["sid"] == sid)
                .unwrap()["in_custom_context"]
                .as_bool()
                .unwrap()
        };
        assert!(in_custom_context(&ddb), "DBT must reconstruct caller {sid}");
        // CLI commands run concurrently and signal delivery has no MI result
        // line. Await the signal itself before inspecting restored state.
        let (status, result) = ddb.api_post_json(
            "/api/v1/commands",
            &serde_json::json!({
                "command": format!("-send-signal --session {sid} SIGCONT"),
                "wait": true,
            }),
        );
        assert!(status.is_success(), "signal delivery failed: {result}");
        assert_eq!(result["data"]["state"], "completed", "{result}");
        let responses = result["data"]["result"]["responses"].as_array().unwrap();
        assert!(!responses.is_empty(), "missing signal response: {result}");
        assert!(
            responses
                .iter()
                .all(|response| response["status"] != "error"),
            "signal delivery failed: {result}"
        );
        assert!(
            !in_custom_context(&ddb),
            "signal resumed caller {sid} without restoring its real registers"
        );
    }
    if let Some(frame_id) = caller_frame {
        let (status, response) = ddb.api_post_json_with_bearer(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListScopes",
            &json!({"frameId": frame_id}),
            V2_TEST_CONTROL_TOKEN,
        );
        assert!(
            !status.is_success(),
            "resumed caller frame must expire: {response}"
        );
        assert!(
            matches!(
                response["code"].as_str(),
                Some("DDB_ERROR_CODE_EXPIRED" | "DDB_ERROR_CODE_FAILED_PRECONDITION")
            ),
            "expected stale/running frame rejection, not an unrelated failure: {response}"
        );
    }
}

#[test]
fn distributed_backtrace_depth_1_stays_local() {
    assert_distributed_backtrace(DebuggerUnderTest::Gdb, 1);
}

#[test]
fn distributed_backtrace_depth_4_walks_across_sessions() {
    assert_distributed_backtrace(DebuggerUnderTest::Gdb, 4);
}

#[test]
fn lldb_distributed_backtrace_depth_1_stays_local() {
    assert_distributed_backtrace(DebuggerUnderTest::Lldb, 1);
}

#[test]
fn lldb_distributed_backtrace_depth_4_walks_across_sessions() {
    assert_distributed_backtrace(DebuggerUnderTest::Lldb, 4);
}
