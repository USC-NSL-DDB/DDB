mod support;

use std::process::{Child, Command};

use support::{
    bkpt_id, build_real_loop_example, real_test_guard, session_id_by_tag, AttachSessionSpec,
    BinarySessionSpec, DdbProcess,
};

struct Debuggee(Child);

impl Drop for Debuggee {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn launches_real_example_under_lldb_and_hits_source_breakpoint() {
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

    let mut ddb = DdbProcess::spawn_lldb_binary_sessions(&[BinarySessionSpec {
        tag: "real-lldb-a",
        alias: "real-lldb-a",
        hash: "grp-real-lldb",
        pid: 9_101,
        ip: "127.0.0.1",
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

    let sid = session_id_by_tag(&sessions, "real-lldb-a");
    ddb.send_cmd(&format!(
        "601-break-insert --session {} {}:{}",
        sid, source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("601^done");

    let bkpts = ddb.api_get("/bkpts");
    let global_bkpt_id = bkpt_id(&bkpts);

    ddb.send_cmd(&format!("602-exec-continue --session {}", sid));
    let sid_needle = format!("session-id=\"{}\"", sid);
    let stop = ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        sid_needle.as_str(),
    ]);
    assert!(stop.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));

    ddb.send_cmd(&format!("603-list-signals --session {}", sid));
    let signals = ddb.wait_for_stdout_line("603^done");
    assert!(signals.contains("signals=["));
    assert!(signals.contains("name=\"SIGINT\""));

    ddb.send_cmd(&format!(
        "604-file-list-lines {} --session {}",
        source_path, sid
    ));
    let lines = ddb.wait_for_stdout_line("604^done");
    assert!(
        lines.contains(&format!("line=\"{}\"", example.breakpoint_line)),
        "LLDB source-line response did not include the breakpoint marker: {lines}"
    );

    ddb.send_cmd(&format!("605-ddb-filter-config --status --session {}", sid));
    let filter_status = ddb.wait_for_stdout_line("605^done");
    assert!(filter_status.contains("message=\"success\""));
    assert!(filter_status.contains("mode=\"blacklist\""));

    ddb.send_cmd(&format!("606-record-time-and-continue --session {}", sid));
    let continued = ddb.wait_for_stdout_line("606^running");
    assert!(
        continued.contains("faketime=\"FAKETIME=-"),
        "record-time command did not update the inferior environment: {continued}"
    );
    ddb.wait_for_stdout_count("reason=\"breakpoint-hit\"", 2);
}

#[test]
fn attaches_to_real_process_under_lldb_and_hits_source_breakpoint() {
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
    let mut debuggee = Debuggee(
        Command::new(binary_path)
            .args(["--sleep-ms", "10", "--max-iterations", "100000"])
            .spawn()
            .expect("real attach fixture should spawn"),
    );
    let pid = u64::from(debuggee.0.id());

    let mut ddb = DdbProcess::spawn_lldb_attach_sessions(&[AttachSessionSpec {
        tag: "real-lldb-attach",
        alias: "real-lldb-attach",
        hash: "grp-real-lldb-attach",
        pid,
        ip: "127.0.0.1",
    }]);

    let sessions = ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let sid = session_id_by_tag(&sessions, "real-lldb-attach");

    ddb.send_cmd(&format!(
        "611-break-insert --session {} {}:{}",
        sid, source_path, example.breakpoint_line
    ));
    ddb.wait_for_stdout_line("611^done");
    ddb.send_cmd(&format!("612-exec-continue --session {}", sid));
    let sid_needle = format!("session-id=\"{}\"", sid);
    ddb.wait_for_stdout_line_with_all(&[
        "*stopped",
        "reason=\"breakpoint-hit\"",
        sid_needle.as_str(),
    ]);

    drop(ddb);
    let status = debuggee
        .0
        .wait()
        .expect("attached fixture should be reaped");
    assert!(
        !status.success(),
        "on_exit=kill should terminate the fixture"
    );
}
