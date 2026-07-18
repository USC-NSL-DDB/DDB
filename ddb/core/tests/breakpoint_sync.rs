mod support;

use support::{bkpt_id, group_id_by_hash, session_id_by_tag, DdbProcess, SessionSpec};

#[test]
fn group_breakpoints_hit_with_global_breakpoint_ids() {
    let mut ddb = DdbProcess::spawn(&[
        SessionSpec {
            tag: "svc-a",
            alias: "api-a",
            hash: "grp-a",
            pid: 301,
            start_delay_ms: 0,
            source_file: "src/a.rs",
            source_line: 40,
            function: "serve_a",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "svc-b",
            alias: "api-b",
            hash: "grp-a",
            pid: 302,
            start_delay_ms: 0,
            source_file: "src/b.rs",
            source_line: 41,
            function: "serve_b",
            exit_on_continue: false,
        },
    ]);

    let sessions = ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);

    let groups = ddb.api_get("/groups");
    let group_id = group_id_by_hash(&groups, "grp-a");
    let sid_a = session_id_by_tag(&sessions, "svc-a");
    let sid_b = session_id_by_tag(&sessions, "svc-b");

    ddb.send_cmd(&format!(
        "201-break-insert --group {} src/main.rs:77",
        group_id
    ));
    ddb.wait_for_stdout_line("201^done");

    let bkpts = ddb.api_get("/bkpts");
    let global_bkpt_id = bkpt_id(&bkpts);
    assert_eq!(
        bkpts["bkpts"]
            .as_array()
            .expect("bkpts should be an array")
            .len(),
        1
    );
    assert_eq!(
        bkpts["bkpts"][0]["subbkpts"][0]["target_group"].as_u64(),
        Some(group_id)
    );
    assert_eq!(
        bkpts["bkpts"][0]["subbkpts"][0]["active_sessions"].as_u64(),
        Some(2)
    );

    ddb.send_cmd(&format!("211-exec-continue --session {}", sid_a));
    let sid_a_needle = format!("session-id=\"{}\"", sid_a);
    let stop_a = ddb.wait_for_stdout_line_with_all(&["*stopped", sid_a_needle.as_str()]);
    assert!(stop_a.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));

    ddb.send_cmd(&format!("212-exec-continue --session {}", sid_b));
    let sid_b_needle = format!("session-id=\"{}\"", sid_b);
    let stop_b = ddb.wait_for_stdout_line_with_all(&["*stopped", sid_b_needle.as_str()]);
    assert!(stop_b.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));
}

#[test]
fn late_joining_session_inherits_group_breakpoints() {
    let mut ddb = DdbProcess::spawn(&[
        SessionSpec {
            tag: "svc-a",
            alias: "api-a",
            hash: "grp-a",
            pid: 401,
            start_delay_ms: 0,
            source_file: "src/a.rs",
            source_line: 50,
            function: "serve_a",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "svc-b",
            alias: "api-b",
            hash: "grp-a",
            pid: 402,
            start_delay_ms: 300,
            source_file: "src/b.rs",
            source_line: 60,
            function: "serve_b",
            exit_on_continue: false,
        },
    ]);

    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);

    let groups = ddb.api_get("/groups");
    let group_id = group_id_by_hash(&groups, "grp-a");

    ddb.send_cmd(&format!(
        "301-break-insert --group {} src/shared.rs:88",
        group_id
    ));
    ddb.wait_for_stdout_line("301^done");

    let initial_bkpts = ddb.api_get("/bkpts");
    let global_bkpt_id = bkpt_id(&initial_bkpts);

    let sessions = ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("thread-created", 2);
    ddb.wait_for_bkpt_active_sessions(global_bkpt_id, 2);

    let sid_b = session_id_by_tag(&sessions, "svc-b");
    ddb.send_cmd(&format!("302-exec-continue --session {}", sid_b));
    let sid_b_needle = format!("session-id=\"{}\"", sid_b);
    let stop_b = ddb.wait_for_stdout_line_with_all(&["*stopped", sid_b_needle.as_str()]);
    assert!(stop_b.contains(&format!("bkptno=\"{}\"", global_bkpt_id)));
}
