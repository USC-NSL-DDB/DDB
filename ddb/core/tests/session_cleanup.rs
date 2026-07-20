mod support;

use support::{session_id_by_tag, DdbProcess, SessionSpec};

#[test]
fn exiting_session_cleans_breakpoints_and_session_state() {
    let mut ddb = DdbProcess::spawn(&[SessionSpec {
        tag: "svc-exit",
        alias: "exit-worker",
        hash: "grp-exit",
        pid: 501,
        start_delay_ms: 0,
        source_file: "src/exit.rs",
        source_line: 70,
        function: "serve_exit",
        exit_on_continue: true,
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    let sid = session_id_by_tag(&sessions, "svc-exit");

    ddb.send_cmd(&format!(
        "401-break-insert --session {} src/exit.rs:70",
        sid
    ));
    ddb.wait_for_stdout_line("401^done");
    assert_eq!(
        ddb.api_get("/bkpts")["bkpts"]
            .as_array()
            .expect("bkpts should be an array")
            .len(),
        1
    );

    ddb.send_cmd(&format!("402-exec-continue --session {}", sid));
    ddb.wait_for_stdout_line("402^running");
    ddb.wait_for_sessions_len(0);
    assert!(ddb.api_get("/bkpts")["bkpts"]
        .as_array()
        .expect("bkpts should be an array")
        .is_empty());
}

#[test]
fn transport_exit_during_bootstrap_never_activates_a_stale_session() {
    let mut ddb = DdbProcess::spawn_with_bootstrap_exit(&[SessionSpec {
        tag: "svc-bootstrap-exit",
        alias: "bootstrap-exit-worker",
        hash: "grp-bootstrap-exit",
        pid: 502,
        start_delay_ms: 0,
        source_file: "src/bootstrap_exit.rs",
        source_line: 71,
        function: "serve_bootstrap_exit",
        exit_on_continue: false,
    }]);

    ddb.wait_for_sessions_len(0);
    assert!(ddb
        .api_get("/groups")
        .as_array()
        .expect("groups should be an array")
        .is_empty());
    assert_eq!(ddb.api_get("/status")["status"], "up");
}
