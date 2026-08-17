mod support;

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN};

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v2-language-sdks",
        alias: "community-clients",
        hash: "api-v2-language-sdk-group",
        pid: 705,
        start_delay_ms: 0,
        source_file: "tests/api_v2_language_sdks.rs",
        source_line: 15,
        function: "mock_session",
        exit_on_continue: false,
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core should be in the DDB workspace")
        .to_path_buf()
}

fn assert_success(language: &str, output: Output) {
    assert!(
        output.status.success(),
        "{language} live SDK smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("SDK smoke output should be UTF-8");
    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("SDK smoke should emit one JSON report");
    assert_eq!(report["language"], language);
    assert_eq!(report["sessions"], 1);
    assert!(report["frames"].as_u64().is_some_and(|count| count > 0));
    assert!(report["serverInstanceId"]
        .as_str()
        .is_some_and(|id| id.starts_with("ddb_")));
}

fn run_with_timeout(command: &mut Command, language: &str) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to start {language} SDK smoke: {error}"));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child
            .try_wait()
            .unwrap_or_else(|error| panic!("failed to poll {language} SDK smoke: {error}"))
            .is_some()
        {
            return child
                .wait_with_output()
                .unwrap_or_else(|error| panic!("failed to collect {language} SDK smoke: {error}"));
        }
        if Instant::now() >= deadline {
            child.kill().unwrap_or_else(|error| {
                panic!("failed to kill stalled {language} SDK smoke: {error}")
            });
            let output = child.wait_with_output().unwrap_or_else(|error| {
                panic!("failed to collect stalled {language} SDK smoke: {error}")
            });
            panic!(
                "{language} SDK smoke exceeded 30 seconds\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "run explicitly after building the TypeScript SDK"]
fn typescript_and_python_sdks_conform_to_a_real_mock_ddb() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let workspace = workspace();
    let endpoint = ddb.api_endpoint();
    let typescript = run_with_timeout(
        Command::new("node")
            .arg(workspace.join("sdk/typescript/test/live-smoke.mjs"))
            .arg(&endpoint)
            .arg(V2_TEST_CONTROL_TOKEN)
            .current_dir(&workspace),
        "typescript",
    );
    assert_success("typescript", typescript);

    let python = run_with_timeout(
        Command::new("python3")
            .arg(workspace.join("sdk/python/tests/live_smoke.py"))
            .arg(&endpoint)
            .arg(V2_TEST_CONTROL_TOKEN)
            .env("PYTHONPATH", workspace.join("sdk/python/src"))
            .current_dir(&workspace),
        "python",
    );
    assert_success("python", python);
}
