mod support;

use support::{session_id_by_tag, DdbProcess, SessionSpec};

#[test]
fn invalid_break_insert_returns_error_without_persisting_breakpoint_state() {
    let mut ddb = DdbProcess::spawn(&[SessionSpec {
        tag: "svc-a",
        alias: "api-a",
        hash: "grp-a",
        pid: 603,
        start_delay_ms: 0,
        source_file: "src/a.rs",
        source_line: 11,
        function: "serve_a",
        exit_on_continue: false,
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    let sid = session_id_by_tag(&sessions, "svc-a");

    ddb.send_cmd(&format!(
        "701-break-insert --session {} not-a-source-location",
        sid
    ));
    let line = ddb.wait_for_stdout_line("701^error");
    assert!(line.contains("Expected <file>:<line>"));
    assert!(ddb.api_get("/bkpts")["bkpts"]
        .as_array()
        .expect("bkpts should be an array")
        .is_empty());
}
