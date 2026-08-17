mod support;

use std::time::Duration;

use ddb_api_conformance::{ConformanceOptions, ConformanceProfile};
use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN};

fn mock_session() -> SessionSpec<'static> {
    SessionSpec {
        tag: "api-v2-conformance",
        alias: "community-client",
        hash: "api-v2-conformance-group",
        pid: 704,
        start_delay_ms: 0,
        source_file: "tests/api_v2_conformance.rs",
        source_line: 12,
        function: "mock_session",
        exit_on_continue: false,
    }
}

#[test]
fn public_sdk_conforms_against_a_real_mock_ddb_process() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[mock_session()]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("thread-created", 1);
    ddb.wait_for_stdout_count("*stopped", 1);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime should build");
    let report = runtime
        .block_on(ddb_api_conformance::run(ConformanceOptions {
            endpoint: ddb.api_endpoint(),
            bearer_token: Some(V2_TEST_CONTROL_TOKEN.to_string()),
            profile: ConformanceProfile::Mock,
            max_collection_items: 1_000,
            connect_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(5),
            stream_timeout: Duration::from_secs(5),
        }))
        .expect("conformance runner should complete");

    assert!(
        report.passed(),
        "public API conformance failed:\n{}",
        serde_json::to_string_pretty(&report).expect("report should serialize")
    );
    assert_eq!(report.profile, ConformanceProfile::Mock);
    assert!(
        report.passed_count() >= 10,
        "report was unexpectedly shallow"
    );
    assert_eq!(report.failed_count(), 0);
}
