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
