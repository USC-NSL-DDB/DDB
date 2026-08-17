mod support;

use support::{DdbProcess, SessionSpec};

#[test]
fn source_endpoints_resolve_debugger_files_to_live_groups() {
    let sessions = [
        SessionSpec {
            tag: "api",
            alias: "api",
            hash: "binary-api",
            pid: 8101,
            start_delay_ms: 0,
            source_file: "src/shared.rs",
            source_line: 11,
            function: "serve",
            exit_on_continue: false,
        },
        SessionSpec {
            tag: "worker",
            alias: "worker",
            hash: "binary-worker",
            pid: 8102,
            start_delay_ms: 0,
            source_file: "src/shared.rs",
            source_line: 22,
            function: "work",
            exit_on_continue: false,
        },
    ];
    let mut ddb = DdbProcess::spawn(&sessions);
    ddb.wait_for_sessions_len(2);

    let mut expected_ids = vec![
        ddb.wait_for_group_id_by_hash("binary-api"),
        ddb.wait_for_group_id_by_hash("binary-worker"),
    ];
    expected_ids.sort_unstable();

    let ids = ddb.api_get("/src_to_grp_ids?src=src%2Fshared.rs");
    let actual_ids = ids["grp_ids"]
        .as_array()
        .expect("source response should contain group ids")
        .iter()
        .map(|id| id.as_u64().expect("group id should be numeric"))
        .collect::<Vec<_>>();
    assert_eq!(actual_ids, expected_ids);

    let resolved_groups = ddb.api_get("/src_to_grps?src=src%2Fshared.rs");
    let mut hashes = resolved_groups["grps"]
        .as_array()
        .expect("source response should contain groups")
        .iter()
        .map(|group| {
            group["hash"]
                .as_str()
                .expect("group hash should be a string")
                .to_owned()
        })
        .collect::<Vec<_>>();
    hashes.sort();
    assert_eq!(hashes, ["binary-api", "binary-worker"]);

    let missing = ddb.api_get("/src_to_grp_ids?src=src%2Fmissing.rs");
    assert_eq!(missing["grp_ids"].as_array().unwrap().len(), 0);
}
