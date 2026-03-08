mod support;

use reqwest::StatusCode;
use serde_json::json;
use support::{session_id_by_tag, DdbProcess, SessionSpec};

#[test]
fn waited_multiple_target_command_returns_all_session_responses() {
    let mut ddb = DdbProcess::spawn(&[
        SessionSpec {
            tag: "svc-a",
            alias: "api-a",
            hash: "grp-a",
            pid: 601,
            start_delay_ms: 0,
            source_file: "src/a.rs",
            source_line: 11,
            function: "serve_a",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "svc-b",
            alias: "api-b",
            hash: "grp-b",
            pid: 602,
            start_delay_ms: 0,
            source_file: "src/b.rs",
            source_line: 22,
            function: "serve_b",
            exit_on_continue: false,
        },
    ]);

    let sessions = ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);

    let sid_a = session_id_by_tag(&sessions, "svc-a");
    let sid_b = session_id_by_tag(&sessions, "svc-b");
    let (status, payload) = ddb.api_post_json(
        "/send",
        &json!({
            "wait": true,
            "cmd": format!("-thread-info --multiple s{},s{}", sid_a, sid_b),
        }),
    );

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["success"], true);

    let mut response_sids = payload["payload"]["responses"]
        .as_array()
        .expect("responses should be an array")
        .iter()
        .map(|response| response["sid"].as_u64().expect("sid should be present"))
        .collect::<Vec<_>>();
    response_sids.sort_unstable();

    assert_eq!(response_sids, vec![sid_a, sid_b]);
}

#[test]
fn waited_broadcast_without_sessions_returns_error_instead_of_hanging() {
    let ddb = DdbProcess::spawn(&[]);

    let (status, payload) = ddb.api_post_json(
        "/send",
        &json!({
            "wait": true,
            "cmd": "-thread-info --all",
        }),
    );

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(payload["success"], false);
    assert!(payload["message"]
        .as_str()
        .expect("message should be a string")
        .contains("No active sessions available for broadcast target"));
}
