mod support;

use std::path::Path;

use support::{
    build_real_dbt_example, capture_session_context, real_test_guard, session_id_by_tag,
    BinarySessionSpec, DdbProcess,
};
use tempfile::tempdir;

const DBT_IP: &str = "127.0.0.1";
const DBT_GROUP: &str = "grp-real-dbt";

fn logical_pid(role_index: usize) -> u64 {
    9_500 + role_index as u64
}

fn session_tag(role_index: usize) -> String {
    format!("{DBT_IP}:-{}", logical_pid(role_index))
}

fn spawn_real_dbt(depth: usize, binary_path: &str, ctx_dir: &Path) -> DdbProcess {
    let tags = (1..=depth).map(session_tag).collect::<Vec<_>>();
    let specs = (1..=depth)
        .map(|role_index| {
            let mut binary_args = vec![
                "--logical-pid".to_string(),
                logical_pid(role_index).to_string(),
                "--role-index".to_string(),
                role_index.to_string(),
                "--self-ctx-file".to_string(),
                ctx_dir
                    .join(format!("ctx-{role_index}.txt"))
                    .to_str()
                    .expect("context path should be valid utf-8")
                    .to_string(),
            ];

            if role_index > 1 {
                binary_args.extend([
                    "--parent-ctx-file".to_string(),
                    ctx_dir
                        .join(format!("ctx-{}.txt", role_index - 1))
                        .to_str()
                        .expect("parent context path should be valid utf-8")
                        .to_string(),
                    "--caller-ip".to_string(),
                    DBT_IP.to_string(),
                    "--caller-pid".to_string(),
                    logical_pid(role_index - 1).to_string(),
                    "--caller-tid".to_string(),
                    "1".to_string(),
                ]);
            }

            BinarySessionSpec {
                tag: tags[role_index - 1].as_str(),
                alias: tags[role_index - 1].as_str(),
                hash: DBT_GROUP,
                pid: logical_pid(role_index),
                ip: DBT_IP,
                start_delay_ms: 0,
                binary_path,
                binary_args,
                stop_at_entry: false,
            }
        })
        .collect::<Vec<_>>();

    DdbProcess::spawn_real_dbt_sessions(&specs)
}

fn extract_first_thread_id(line: &str) -> u64 {
    let (_, threads) = line.split_once("threads=[").unwrap_or_else(|| {
        panic!("thread-info output should include a threads payload, got: {line}");
    });

    for (offset, _) in threads.match_indices("id=\"") {
        if offset == 0 {
            continue;
        }
        let prefix = threads.as_bytes()[offset - 1];
        if prefix != b'{' && prefix != b',' {
            continue;
        }
        let rest = &threads[offset + 4..];
        let id = rest
            .split('"')
            .next()
            .expect("thread id should terminate with a quote");
        return id.parse::<u64>().unwrap_or_else(|error| {
            panic!("thread id should be a valid integer in `{line}`: {error}");
        });
    }

    panic!("thread-info output should include a thread id, got: {line}");
}

fn resolve_single_thread_gtid(ddb: &mut DdbProcess, sid: u64) -> u64 {
    let token = 8_000 + sid;
    ddb.send_cmd(&format!("{token}-thread-info --session {sid}"));
    let output = ddb.wait_for_stdout_line(&format!("{token}^done"));
    extract_first_thread_id(&output)
}

fn write_context_file(
    ctx_dir: &Path,
    role_index: usize,
    context: &std::collections::BTreeMap<String, u64>,
) {
    let payload = ["pc", "sp", "fp", "lr"]
        .into_iter()
        .filter_map(|register| {
            context
                .get(register)
                .map(|value| format!("{register}={value}\n"))
        })
        .collect::<String>();
    std::fs::write(ctx_dir.join(format!("ctx-{role_index}.txt")), payload)
        .expect("context file should be written");
}

fn assert_distributed_backtrace(depth: usize) {
    let _guard = real_test_guard();
    let example = build_real_dbt_example();
    let binary_path = example
        .binary_path
        .to_str()
        .expect("fixture binary path should be valid utf-8");
    let ctx_dir = tempdir().expect("temporary context directory should be created");
    let mut ddb = spawn_real_dbt(depth, binary_path, ctx_dir.path());

    let sessions = ddb.wait_for_sessions_len(depth);
    ddb.wait_for_stdout_count("thread-created", depth);
    for role_index in 1..=depth {
        ddb.wait_for_stdout_count("*stopped", role_index);
        let sid = session_id_by_tag(&sessions, &session_tag(role_index));
        let context = capture_session_context(&ddb, sid);
        write_context_file(ctx_dir.path(), role_index, &context);
    }

    let leaf_sid = session_id_by_tag(&sessions, &session_tag(depth));
    let root_sid = session_id_by_tag(&sessions, &session_tag(1));
    let leaf_gtid = resolve_single_thread_gtid(&mut ddb, leaf_sid);

    if depth > 1 {
        let token = 8_500 + depth as u64;
        ddb.send_cmd(&format!("{token}-get-remote-bt --thread {leaf_gtid}"));
        let output = ddb.wait_for_stdout_line(&format!("{token}^done"));
        assert!(
            output.contains("message=\"success\""),
            "remote-bt metadata lookup failed for depth {depth}: {output}"
        );
    }

    let token = 9_000 + depth as u64;
    ddb.send_cmd(&format!("{token}-bt-remote --thread {leaf_gtid}"));
    let output = ddb.wait_for_stdout_line(&format!("{token}^done"));

    assert!(
        output.contains("stack=["),
        "unexpected DBT output: {output}"
    );
    assert!(
        output.contains(&format!("session=\"{leaf_sid}\"")),
        "leaf session {leaf_sid} missing from DBT output: {output}"
    );
    assert!(
        output.contains(&format!("session=\"{root_sid}\"")),
        "root session {root_sid} missing from DBT output: {output}"
    );
    assert_eq!(
        output.matches("boundary_frame=\"1\"").count(),
        depth.saturating_sub(1),
        "unexpected boundary-frame count for depth {depth}: {output}"
    );
}

#[test]
fn distributed_backtrace_depth_1_stays_local() {
    assert_distributed_backtrace(1);
}

#[test]
fn distributed_backtrace_depth_4_walks_across_sessions() {
    assert_distributed_backtrace(4);
}
