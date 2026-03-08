mod support;

use support::{
    bkpt_id, build_real_loop_example, group_id_by_hash, real_test_guard, session_id_by_tag,
    BinarySessionSpec, DdbProcess,
};

#[test]
fn real_binary_session_late_join_inherits_group_breakpoints() {
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

    let mut ddb = DdbProcess::spawn_real_binary_sessions(&[
        BinarySessionSpec {
            tag: "real-a",
            alias: "real-a",
            hash: "grp-real",
            pid: 9101,
            start_delay_ms: 0,
            binary_path,
            binary_args: vec![
                "--sleep-ms".to_string(),
                "15".to_string(),
                "--max-iterations".to_string(),
                "100000".to_string(),
            ],
            stop_at_entry: true,
        },
        BinarySessionSpec {
            tag: "real-b",
            alias: "real-b",
            hash: "grp-real",
            pid: 9102,
            start_delay_ms: 300,
            binary_path,
            binary_args: vec![
                "--sleep-ms".to_string(),
                "15".to_string(),
                "--max-iterations".to_string(),
                "100000".to_string(),
            ],
            stop_at_entry: true,
        },
    ]);

    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);

    let groups = ddb.api_get("/groups");
    let group_id = group_id_by_hash(&groups, "grp-real");

    ddb.send_cmd(&format!(
        "601-break-insert --group {} {}:{}",
        group_id, source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("601^done");

    let initial_bkpts = ddb.api_get("/bkpts");
    let global_bkpt_id = bkpt_id(&initial_bkpts);

    let sessions = ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);
    ddb.wait_for_bkpt_active_sessions(global_bkpt_id, 2);

    let sid_a = session_id_by_tag(&sessions, "real-a");
    let sid_b = session_id_by_tag(&sessions, "real-b");

    ddb.send_cmd(&format!("602-exec-continue --session {}", sid_b));
    let sid_b_needle = format!("session-id=\"{}\"", sid_b);
    let stop_b = ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        sid_b_needle.as_str(),
    ]);
    assert!(stop_b.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));

    ddb.send_cmd(&format!("603-exec-continue --session {}", sid_a));
    let sid_a_needle = format!("session-id=\"{}\"", sid_a);
    let stop_a = ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        sid_a_needle.as_str(),
    ]);
    assert!(stop_a.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));
}
