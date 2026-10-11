mod support;

use ddb_api_client::{v2, ClientConfig, ClientError, DdbClient};
use support::{DdbProcess, SessionSpec, V2_TEST_CONTROL_TOKEN, V2_TEST_READ_TOKEN};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn client(ddb: &DdbProcess) -> DdbClient {
    DdbClient::new(ClientConfig::new(ddb.api_endpoint()).with_bearer_token(V2_TEST_READ_TOKEN))
        .unwrap()
}

fn root_yaml(name: &str, path: &std::path::Path, tags: &[&str]) -> String {
    format!(
        "  - name: {name}\n    path: {}\n    debugger_path: /build\n    session_tags: {}\n",
        serde_json::to_string(path.to_str().unwrap()).unwrap(),
        serde_json::to_string(tags).unwrap(),
    )
}

fn code(error: ClientError) -> v2::DdbErrorCode {
    let ClientError::Api { error, .. } = error else {
        panic!("expected API error: {error}")
    };
    v2::DdbErrorCode::try_from(error.code).unwrap()
}

fn session<'a>(tag: &'a str, pid: u32) -> SessionSpec<'a> {
    SessionSpec {
        tag,
        alias: tag,
        hash: "same-distributed-binary",
        pid: pid.into(),
        start_delay_ms: 0,
        source_file: "/build/main.rs",
        source_line: 1,
        function: "main",
        exit_on_continue: false,
    }
}

#[test]
fn source_targets_include_only_exact_catalog_owners_without_requiring_content() {
    let mut other = session("other", 8593);
    other.hash = "unrelated-binary";
    other.source_file = "/other/main.rs";
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[session("a", 8591), session("b", 8592), other]);
    ddb.wait_for_sessions_len(3);
    ddb.wait_for_stdout_count("*stopped", 3);
    let client = client(&ddb);
    runtime().block_on(async {
        let sessions = client
            .collect_sessions(Default::default(), 10)
            .await
            .unwrap();
        let matching = sessions
            .iter()
            .filter(|session| session.tag != "other")
            .map(|session| session.session_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let group_id = sessions
            .iter()
            .find(|session| session.tag == "a")
            .unwrap()
            .group_id
            .clone()
            .unwrap();
        let result = client
            .resolve_source_targets(v2::ResolveSourceTargetsRequest {
                source: "/build/main.rs".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(result.complete, "{:?}", result.warnings);
        assert!(result.warnings.is_empty());
        assert_eq!(result.targets.len(), 3, "two sessions and their group only");
        assert!(
            matches!(result.targets[0].target.as_ref().unwrap().selector.as_ref(),
            Some(v2::target::Selector::Group(group)) if group.group_id == group_id)
        );
        assert_eq!(result.targets[0].matching_session_count, 2);
        let returned = result
            .targets
            .iter()
            .filter_map(
                |entry| match entry.target.as_ref().unwrap().selector.as_ref().unwrap() {
                    v2::target::Selector::Session(session) => Some(session.session_id.clone()),
                    v2::target::Selector::Group(_) => None,
                    other => panic!("unexpected unfiltered scope: {other:?}"),
                },
            )
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(returned, matching);
        let one = matching.first().unwrap();
        let scoped = client
            .resolve_source_targets(v2::ResolveSourceTargetsRequest {
                source: "/build/main.rs".into(),
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                        session_id: one.clone(),
                    })),
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(scoped.targets.len(), 2);
        assert!(scoped
            .targets
            .iter()
            .all(|entry| entry.matching_session_count == 1));
        for source in ["main.rs", "/build/absent.rs"] {
            let absent = client
                .resolve_source_targets(v2::ResolveSourceTargetsRequest {
                    source: source.into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert!(absent.complete, "{:?}", absent.warnings);
            assert!(
                absent.targets.is_empty(),
                "basenames/suffixes must not infer ownership"
            );
        }
        for source in ["", "\n", &"x".repeat(4097)] {
            assert_eq!(
                code(
                    client
                        .resolve_source_targets(v2::ResolveSourceTargetsRequest {
                            source: source.into(),
                            ..Default::default()
                        })
                        .await
                        .unwrap_err()
                ),
                v2::DdbErrorCode::InvalidArgument
            );
        }
        let anonymous = DdbClient::new(ClientConfig::new(ddb.api_endpoint())).unwrap();
        assert_eq!(
            code(
                anonymous
                    .resolve_source_targets(v2::ResolveSourceTargetsRequest {
                        source: "/build/main.rs".into(),
                        ..Default::default()
                    })
                    .await
                    .unwrap_err()
            ),
            v2::DdbErrorCode::Unauthenticated
        );
        let other = sessions
            .iter()
            .find(|session| session.tag == "other")
            .unwrap();
        let control = DdbClient::new(
            ClientConfig::new(ddb.api_endpoint()).with_bearer_token(V2_TEST_CONTROL_TOKEN),
        )
        .unwrap();
        control
            .execute(v2::ExecuteRequest {
                target: Some(v2::Target {
                    selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                        session_id: other.session_id.clone(),
                    })),
                }),
                action: v2::ExecutionAction::Continue as i32,
                ..Default::default()
            })
            .await
            .unwrap();
        ddb.wait_for_stdout_count("*running", 1);
        let cached = client
            .resolve_source_targets(v2::ResolveSourceTargetsRequest {
                source: "/build/main.rs".into(),
                refresh: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            cached.complete,
            "known running negatives should remain usable"
        );
        assert_eq!(cached.targets.len(), 3);
    });
}

#[test]
fn workspace_browsing_needs_no_session_and_reads_only_explicit_exports() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("src")).unwrap();
    std::fs::write(directory.path().join("notes.txt"), "first\n\nlast\n").unwrap();
    std::fs::write(directory.path().join(".hidden"), "hidden").unwrap();
    let ddb = DdbProcess::spawn_with_source_roots(
        &[],
        &format!(
            "\nSourceRoots:\n{}",
            root_yaml("project", directory.path(), &[])
        ),
    );
    let client = client(&ddb);
    runtime().block_on(async {
        let roots = client
            .collect_source_roots(Default::default(), 32)
            .await
            .unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "project");
        assert!(roots[0].owners.is_empty());
        let request = v2::ListSourceEntriesRequest {
            root_reference: roots[0].root_reference.clone(),
            ..Default::default()
        };
        let listing = client.list_source_entries(request.clone()).await.unwrap();
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["src", "notes.txt"]
        );
        let source = listing.entries[1].source.as_ref().unwrap();
        assert_eq!(source.origin, v2::SourceOrigin::Workspace as i32);
        assert_eq!(source.debugger_path.as_deref(), Some("/build/notes.txt"));
        let window = client
            .read_source(v2::ReadSourceRequest {
                source_reference: source.source_reference.clone(),
                start_line: 2,
                max_lines: 1,
                ..Default::default()
            })
            .await
            .unwrap()
            .source
            .unwrap();
        assert_eq!(window.content, "");
        assert_eq!(window.line_count, 1);
        assert_eq!(window.total_lines, 3);
        assert!(window.has_more);
        let beyond = client
            .read_source(v2::ReadSourceRequest {
                source_reference: source.source_reference.clone(),
                start_line: 90,
                max_lines: 1,
                ..Default::default()
            })
            .await
            .unwrap()
            .source
            .unwrap();
        assert_eq!(beyond.line_count, 0);
        assert_eq!(beyond.total_lines, 3);
        let all = client
            .list_source_entries(v2::ListSourceEntriesRequest {
                include_hidden: true,
                ..request.clone()
            })
            .await
            .unwrap();
        assert_eq!(all.entries.len(), 3);
        let paged = v2::ListSourceEntriesRequest {
            page: Some(v2::PageRequest {
                page_size: 1,
                page_token: None,
            }),
            ..request.clone()
        };
        let first_page = client.list_source_entries(paged.clone()).await.unwrap();
        let token = first_page.page.unwrap().next_page_token.unwrap();
        let second_page_request = v2::ListSourceEntriesRequest {
            page: Some(v2::PageRequest {
                page_size: 1,
                page_token: Some(token),
            }),
            ..request.clone()
        };
        let second_page = client
            .list_source_entries(second_page_request.clone())
            .await
            .unwrap();
        assert_eq!(second_page.entries[0].name, "notes.txt");
        std::fs::write(directory.path().join("another.txt"), "new").unwrap();
        let refreshed = client.list_source_entries(request.clone()).await.unwrap();
        assert_ne!(first_page.revision, refreshed.revision);
        assert_eq!(
            code(
                client
                    .list_source_entries(second_page_request)
                    .await
                    .unwrap_err()
            ),
            v2::DdbErrorCode::Expired
        );
        for path in ["..", "../outside", "/etc"] {
            let error = client
                .list_source_entries(v2::ListSourceEntriesRequest {
                    directory: path.into(),
                    ..request.clone()
                })
                .await
                .unwrap_err();
            assert_eq!(code(error), v2::DdbErrorCode::InvalidArgument);
        }
        let error = client
            .resolve_source(v2::ResolveSourceRequest {
                location: Some(v2::SourceLocation {
                    path: Some(directory.path().join("notes.txt").to_string_lossy().into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(code(error), v2::DdbErrorCode::NotFound);
        let unauthenticated = DdbClient::new(ClientConfig::new(ddb.api_endpoint())).unwrap();
        assert_eq!(
            code(
                unauthenticated
                    .list_source_roots(Default::default())
                    .await
                    .unwrap_err()
            ),
            v2::DdbErrorCode::Unauthenticated
        );
    });
}

#[test]
fn distributed_catalog_keeps_session_ownership_and_applies_session_mappings() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("main.rs"), "version a\n").unwrap();
    std::fs::write(b.path().join("main.rs"), "version b\n").unwrap();
    let config = format!(
        "\nSourceRoots:\n{}{}",
        root_yaml("a", a.path(), &["a"]),
        root_yaml("b", b.path(), &["b"])
    );
    let mut ddb =
        DdbProcess::spawn_with_source_roots(&[session("a", 8501), session("b", 8502)], &config);
    ddb.wait_for_sessions_len(2);
    ddb.wait_for_stdout_count("*stopped", 2);
    let client = client(&ddb);
    runtime().block_on(async {
        let sessions = client
            .collect_sessions(Default::default(), 10)
            .await
            .unwrap();
        let mut tags = sessions
            .iter()
            .map(|session| session.tag.as_str())
            .collect::<Vec<_>>();
        tags.sort_unstable();
        assert_eq!(tags, ["a", "b"]);
        assert!(sessions
            .iter()
            .all(|session| session.session_id != session.tag));
        let catalog = client
            .collect_sources(
                v2::ListSourcesRequest {
                    refresh: true,
                    page: Some(v2::PageRequest {
                        page_size: 1,
                        page_token: None,
                    }),
                    ..Default::default()
                },
                10,
            )
            .await
            .unwrap();
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        assert_eq!(catalog.sources.len(), 2);
        let sources = &catalog.sources;
        assert_ne!(sources[0].source_reference, sources[1].source_reference);
        assert_ne!(sources[0].owners, sources[1].owners);
        assert_eq!(sources[0].path, sources[1].path);
        assert_eq!(sources[0].owners[0].group_id, sources[1].owners[0].group_id);
        let ambiguous = client
            .resolve_source(v2::ResolveSourceRequest {
                location: Some(v2::SourceLocation {
                    path: Some("/build/main.rs".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(code(ambiguous), v2::DdbErrorCode::Conflict);
        let mut texts = Vec::new();
        for source in sources {
            assert_eq!(
                source.availability,
                v2::SourceAvailability::Available as i32
            );
            let result = client
                .read_source(v2::ReadSourceRequest {
                    source_reference: source.source_reference.clone(),
                    start_line: 1,
                    max_lines: 10,
                    ..Default::default()
                })
                .await
                .unwrap()
                .source
                .unwrap();
            texts.push(result.content);
        }
        texts.sort();
        assert_eq!(texts, ["version a", "version b"]);
        let wrong_target = v2::Target {
            selector: Some(v2::target::Selector::Session(v2::SessionTarget {
                session_id: sources[1].owners[0].session_id.clone(),
            })),
        };
        let error = client
            .resolve_source(v2::ResolveSourceRequest {
                target: Some(wrong_target),
                location: Some(v2::SourceLocation {
                    source_reference: Some(sources[0].source_reference.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(code(error), v2::DdbErrorCode::NotFound);
        let filtered = client
            .list_sources(v2::ListSourcesRequest {
                query: "MAIN.RS".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.sources.len(), 2);
    });
}

#[test]
fn missing_debugger_sources_remain_discoverable() {
    let mut ddb = DdbProcess::spawn_with_v2_auth(&[session("missing", 8503)]);
    ddb.wait_for_sessions_len(1);
    ddb.wait_for_stdout_count("*stopped", 1);
    let client = client(&ddb);
    runtime().block_on(async {
        let listing = client.list_sources(Default::default()).await.unwrap();
        assert_eq!(listing.sources.len(), 1);
        let source = &listing.sources[0];
        assert_eq!(
            source.availability,
            v2::SourceAvailability::Unavailable as i32
        );
        assert!(source.unavailable_reason.is_some());
        let error = client
            .read_source(v2::ReadSourceRequest {
                source_reference: source.source_reference.clone(),
                start_line: 1,
                max_lines: 1,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(code(error), v2::DdbErrorCode::NotFound);
    });
}
