mod support;

use support::{
    build_real_loop_example, real_test_guard, session_id_by_tag, BinarySessionSpec, DdbProcess,
};

#[test]
fn real_binary_session_exit_is_reflected_in_ddb_state() {
    let _guard = real_test_guard();
    let example = build_real_loop_example();
    let binary_path = example
        .binary_path
        .to_str()
        .expect("fixture binary path should be valid utf-8");

    let mut ddb = DdbProcess::spawn_real_binary_sessions(&[BinarySessionSpec {
        tag: "real-exit",
        alias: "real-exit",
        hash: "grp-real-exit",
        pid: 9201,
        ip: "127.0.0.1",
        start_delay_ms: 0,
        binary_path,
        binary_args: vec![
            "--sleep-ms".to_string(),
            "5".to_string(),
            "--max-iterations".to_string(),
            "5".to_string(),
        ],
        stop_at_entry: true,
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let sid = session_id_by_tag(&sessions, "real-exit");
    ddb.send_cmd(&format!("701-exec-continue --session {}", sid));
    ddb.wait_for_stdout_line("701^running");
    ddb.wait_for_sessions_len(0);
}
