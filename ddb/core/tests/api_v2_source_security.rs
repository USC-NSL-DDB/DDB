#![cfg(unix)]

mod support;

use ddb_api_client::{v2, ClientConfig, ClientError, DdbClient};
use std::os::unix::fs::{symlink, PermissionsExt};
use support::{DdbProcess, V2_TEST_READ_TOKEN};

fn code(error: ClientError) -> v2::DdbErrorCode {
    match error {
        ClientError::Api { error, .. } => v2::DdbErrorCode::try_from(error.code).unwrap(),
        other => panic!("expected an API error, got {other}"),
    }
}

#[test]
fn cached_source_reads_recheck_permissions_and_symlink_confinement() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let allowed = directory.path().join("allowed.rs");
    std::fs::write(&allowed, "allowed").unwrap();
    std::fs::write(outside.path().join("secret"), "secret").unwrap();
    symlink("allowed.rs", directory.path().join("link.rs")).unwrap();
    let config = format!(
        "\nSourceRoots:\n  - name: project\n    path: {}\n",
        serde_json::to_string(directory.path().to_str().unwrap()).unwrap()
    );
    let ddb = DdbProcess::spawn_with_source_roots(&[], &config);
    let client =
        DdbClient::new(ClientConfig::new(ddb.api_endpoint()).with_bearer_token(V2_TEST_READ_TOKEN))
            .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = client
            .list_source_roots(Default::default())
            .await
            .unwrap()
            .roots
            .remove(0);
        let source = client
            .resolve_source(v2::ResolveSourceRequest {
                root_reference: Some(root.root_reference.clone()),
                location: Some(v2::SourceLocation {
                    path: Some("link.rs".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap()
            .source
            .unwrap();
        let read = v2::ReadSourceRequest {
            source_reference: source.source_reference,
            start_line: 1,
            max_lines: 1,
            ..Default::default()
        };
        assert_eq!(
            client
                .read_source(read.clone())
                .await
                .unwrap()
                .source
                .unwrap()
                .content,
            "allowed"
        );
        std::fs::set_permissions(&allowed, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Superuser runners can still read a mode-000 file. Confinement below
        // is mandatory on every host; permission denial follows host policy.
        if std::fs::read(&allowed).is_err() {
            assert_eq!(
                code(client.read_source(read.clone()).await.unwrap_err()),
                v2::DdbErrorCode::PermissionDenied
            );
        }
        std::fs::set_permissions(&allowed, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(directory.path().join("link.rs")).unwrap();
        symlink(
            outside.path().join("secret"),
            directory.path().join("link.rs"),
        )
        .unwrap();
        assert_eq!(
            code(client.read_source(read).await.unwrap_err()),
            v2::DdbErrorCode::PermissionDenied
        );
        symlink(outside.path(), directory.path().join("escape")).unwrap();
        assert_eq!(
            code(
                client
                    .list_source_entries(v2::ListSourceEntriesRequest {
                        root_reference: root.root_reference,
                        directory: "escape".into(),
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            v2::DdbErrorCode::PermissionDenied
        );
    });
}
