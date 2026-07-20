mod support;

use reqwest::StatusCode;
use serde_json::json;
use support::{session_id_by_tag, DdbProcess, SessionSpec};

#[test]
fn execution_commands_share_the_waited_api_path() {
    let mut ddb = DdbProcess::spawn(&[SessionSpec {
        tag: "svc-a",
        alias: "api-a",
        hash: "grp-a",
        pid: 651,
        start_delay_ms: 0,
        source_file: "src/a.rs",
        source_line: 11,
        function: "serve_a",
        exit_on_continue: false,
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    let session_id = session_id_by_tag(&sessions, "svc-a");

    for command in [
        "-exec-step --thread 1".to_string(),
        format!("-exec-interrupt --session {}", session_id),
        format!("-send-signal SIGUSR1 --session {}", session_id),
        format!("-list-signals --session {}", session_id),
    ] {
        let (status, response) = ddb.api_post_json(
            "/send",
            &json!({
                "wait": true,
                "cmd": command,
            }),
        );

        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(response["success"], true, "{response:?}");
        assert_eq!(
            response["payload"]["responses"][0]["sid"].as_u64(),
            Some(session_id),
            "{response:?}"
        );
    }
}

#[test]
fn stepping_without_a_thread_target_returns_a_specific_error() {
    let mut ddb = DdbProcess::spawn(&[SessionSpec {
        tag: "svc-a",
        alias: "api-a",
        hash: "grp-a",
        pid: 652,
        start_delay_ms: 0,
        source_file: "src/a.rs",
        source_line: 11,
        function: "serve_a",
        exit_on_continue: false,
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    let session_id = session_id_by_tag(&sessions, "svc-a");
    let (status, response) = ddb.api_post_json(
        "/send",
        &json!({
            "wait": true,
            "cmd": format!("-exec-step --session {}", session_id),
        }),
    );

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response["success"], false);
    assert!(response["message"]
        .as_str()
        .expect("error message should be a string")
        .contains("exec-step command should specify a thread id"));
}
