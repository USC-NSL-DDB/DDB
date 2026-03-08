mod support;

use support::{
    bkpt_id, build_real_loop_example, real_test_guard, session_id_by_tag, BinarySessionSpec,
    DdbProcess,
};

#[test]
fn launches_real_example_under_gdb_and_hits_source_breakpoint() {
    let _guard = real_test_guard();
    let example = build_real_loop_example();
    let binary_path = example
        .binary_path
        .to_str()
        .expect("fixture binary path should be valid utf-8");
    let source_path = example
        .source_path
        .to_str()
        .expect("fixture source path should be valid utf-8");

    let mut ddb = DdbProcess::spawn_real_binary_sessions(&[BinarySessionSpec {
        tag: "real-a",
        alias: "real-a",
        hash: "grp-real",
        pid: 9001,
        start_delay_ms: 0,
        binary_path,
        binary_args: vec![
            "--sleep-ms".to_string(),
            "10".to_string(),
            "--max-iterations".to_string(),
            "100000".to_string(),
        ],
        stop_at_entry: true,
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let sid = session_id_by_tag(&sessions, "real-a");
    ddb.send_cmd(&format!(
        "501-break-insert --session {} {}:{}",
        sid, source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("501^done");

    let bkpts = ddb.api_get("/bkpts");
    let global_bkpt_id = bkpt_id(&bkpts);

    ddb.send_cmd(&format!("502-exec-continue --session {}", sid));
    let sid_needle = format!("session-id=\"{}\"", sid);
    let stop = ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        sid_needle.as_str(),
    ]);
    assert!(stop.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));
}
