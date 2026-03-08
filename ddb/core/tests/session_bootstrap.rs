mod support;

use support::{session_id_by_tag, DdbProcess, SessionSpec};

#[test]
fn boots_static_sessions_and_reports_thread_info() {
    let mut ddb = DdbProcess::spawn(&[
        SessionSpec {
            tag: "svc-a",
            alias: "api-a",
            hash: "grp-a",
            pid: 101,
            start_delay_ms: 0,
            source_file: "src/api.rs",
            source_line: 11,
            function: "serve_a",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "svc-b",
            alias: "api-b",
            hash: "grp-a",
            pid: 102,
            start_delay_ms: 0,
            source_file: "src/api.rs",
            source_line: 22,
            function: "serve_b",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "svc-c",
            alias: "worker",
            hash: "grp-b",
            pid: 201,
            start_delay_ms: 0,
            source_file: "src/worker.rs",
            source_line: 33,
            function: "serve_c",
            exit_on_continue: false,
        },
    ]);

    let sessions = ddb.wait_for_sessions_len(3);
    ddb.wait_for_stdout_count("thread-created", 3);

    let groups = ddb.api_get("/groups");
    assert_eq!(groups.as_array().expect("groups should be an array").len(), 2);
    let mut group_sizes = groups
        .as_array()
        .expect("groups should be an array")
        .iter()
        .map(|group| group["sids"].as_array().expect("sids should be an array").len())
        .collect::<Vec<_>>();
    group_sizes.sort_unstable();
    assert_eq!(group_sizes, vec![1, 2]);

    assert!(session_id_by_tag(&sessions, "svc-a") > 0);
    assert!(session_id_by_tag(&sessions, "svc-b") > 0);
    assert!(session_id_by_tag(&sessions, "svc-c") > 0);

    ddb.send_cmd("101-thread-info");
    let output = ddb.wait_for_stdout_line("101^done");
    assert!(output.contains("threads=["));
    assert!(output.contains("id=\"1\""));
    assert!(output.contains("id=\"2\""));
    assert!(output.contains("id=\"3\""));
}
