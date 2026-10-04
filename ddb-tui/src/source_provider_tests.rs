use super::*;
use crate::{
    api::ClientConfig,
    source_browser::SourceBrowser,
    source_explorer::{Action, Explorer},
    source_types::SourceId,
};
use serde_json::json;
use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn config(directory: &tempfile::TempDir, roots: serde_json::Value) -> Result<Workspace> {
    let path = directory.path().join("workspace.yaml");
    fs::write(
        &path,
        serde_json::to_vec(&json!({ "roots": roots })).unwrap(),
    )
    .unwrap();
    Workspace::load(&path)
}

fn root(name: &str, prefix: &str, tags: &[&str]) -> serde_json::Value {
    json!({"name": name, "path": "checkout", "debugger_path": prefix, "session_tags": tags})
}

fn checkout() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("checkout")).unwrap();
    directory
}

fn session(id: &str, tag: &str) -> v2::Session {
    v2::Session {
        session_id: id.into(),
        tag: tag.into(),
        display_name: "a label unrelated to the tag".into(),
        ..Default::default()
    }
}

fn source(path: &str, owners: &[&str]) -> Source {
    Source::Ddb {
        instance: "test".into(),
        file: v2::SourceFile {
            source_reference: "ref".into(),
            debugger_path: Some(path.into()),
            owners: owners
                .iter()
                .map(|id| v2::SourceOwner {
                    session_id: (*id).into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    }
}

fn local(root: LocalRoot, path: &str) -> LocalSource {
    LocalSource {
        root,
        path: path.into(),
        owner_hint: Vec::new(),
        owner_instance: None,
    }
}

fn disconnected_client() -> DdbClient {
    DdbClient::new(ClientConfig::new("http://127.0.0.1:1")).unwrap()
}

#[test]
fn configuration_requires_explicit_valid_grants_and_resolves_paths_from_its_file() {
    let directory = checkout();
    assert!(Workspace::default().roots().is_empty());
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    assert_eq!(workspace.roots()[0].name, "app");
    for roots in [
        json!([root("app", "/build", &[]), root("app", "/other", &[])]),
        json!([root("bad/name", "/build", &[])]),
        json!([root("app", "relative", &[])]),
        json!([root("app", "//", &[])]),
        json!([root("app", "/build//app", &[])]),
        json!([root("app", "/build/../other", &[])]),
        json!([root("app", "/build", &["a", "a"])]),
        json!([{"name":"app", "path":""}]),
        json!([{"name":"app", "path":"missing"}]),
        json!([{"name":"app", "path":"checkout", "typo":true}]),
        serde_json::Value::Array(
            (0..33)
                .map(|n| root(&format!("root-{n}"), "/build", &[]))
                .collect(),
        ),
    ] {
        assert!(config(&directory, roots).is_err());
    }
}

#[test]
fn mappings_use_longest_boundary_prefix_and_canonical_tags_not_display_labels() {
    let directory = checkout();
    let workspace = config(
        &directory,
        json!([
            root("global", "/build", &[]),
            root("alpha", "/build", &["alpha"]),
            root("beta", "/build", &["beta"]),
            root("nested", "/build/lib", &[])
        ]),
    )
    .unwrap();
    let sessions = [session("opaque-a", "alpha"), session("opaque-b", "beta")];
    workspace.update_sessions(&sessions);
    for (path, owner, expected, relative) in [
        ("/build/main.rs", "opaque-a", "alpha", "main.rs"),
        ("/build/main.rs", "opaque-b", "beta", "main.rs"),
        ("/build/main.rs", "unknown", "global", "main.rs"),
        ("/build/lib/worker.rs", "opaque-a", "nested", "worker.rs"),
    ] {
        let mapped = workspace.map_source(source(path, &[owner])).unwrap();
        assert_eq!(
            mapped.id(),
            SourceId::Local {
                root: expected.into(),
                path: relative.into()
            }
        );
        assert_eq!(mapped.debugger_path().as_deref(), Some(path));
        assert_eq!(mapped.owners(&sessions)[0].session_id, owner);
    }
    assert!(!workspace
        .map_source(source("/builder/main.rs", &["opaque-a"]))
        .unwrap()
        .is_local());
    // A descriptor owned by two sessions cannot arbitrarily pick one checkout.
    let mapped = workspace
        .map_source(source("/build/main.rs", &["opaque-a", "opaque-b"]))
        .unwrap();
    assert_eq!(
        mapped.id(),
        SourceId::Local {
            root: "global".into(),
            path: "main.rs".into()
        }
    );
}

#[test]
fn ambiguous_mapping_is_an_error_even_when_one_candidate_file_exists() {
    let directory = checkout();
    let workspace = config(
        &directory,
        json!([root("one", "/build", &[]), root("two", "/build", &[])]),
    )
    .unwrap();
    assert!(workspace
        .map_source(source("/build/main.rs", &[]))
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
}

#[tokio::test]
async fn local_files_and_paged_directories_work_without_any_backend_or_session() {
    let directory = checkout();
    let workspace = config(&directory, json!([{"name":"local", "path":"checkout"}])).unwrap();
    for n in 0..205 {
        fs::write(
            directory.path().join(format!("checkout/{n:03}.txt")),
            "one\n\ntwo\n",
        )
        .unwrap();
    }
    fs::write(directory.path().join("checkout/.hidden"), "hidden").unwrap();
    let client = disconnected_client();
    let mut explorer = Explorer::default();
    let request = explorer.open(false);
    let page = discover(&client, &workspace, request.clone())
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.warnings.len(), 1);
    explorer.apply(request.generation, page);
    let Some(Action::Request(request)) = explorer.activate() else {
        panic!("root")
    };
    let first = discover(&client, &workspace, request.clone())
        .await
        .unwrap();
    assert_eq!(first.entries.len(), 100);
    let Entry::Source(file) = &first.entries[0] else {
        panic!("file")
    };
    let mut browser = SourceBrowser::default();
    let content = read(&client, &workspace, browser.open_source(file.clone()))
        .await
        .unwrap();
    assert_eq!(content.content, "one\n\ntwo");
    assert_eq!(content.total_lines, 3);
    assert!(
        content.source.debugger_path().is_none(),
        "unmapped files must not imply breakpoint paths"
    );
    let first_token = first.next_token.clone().unwrap();
    let mut next = request.clone();
    next.page_token = Some(first_token.clone());
    let second = discover(&client, &workspace, next.clone()).await.unwrap();
    assert_eq!(second.entries.len(), 100);
    next.page_token = second.next_token;
    let last = discover(&client, &workspace, next).await.unwrap();
    assert_eq!(last.entries.len(), 5);
    assert!(last.next_token.is_none());
    fs::write(directory.path().join("checkout/new"), "new").unwrap();
    discover(&client, &workspace, request.clone())
        .await
        .unwrap(); // fresh listing invalidates old token
    let mut stale = request.clone();
    stale.page_token = Some(first_token);
    assert!(discover(&client, &workspace, stale.clone())
        .await
        .unwrap_err()
        .to_string()
        .contains("changed"));
    stale.include_hidden = true;
    assert!(discover(&client, &workspace, stale)
        .await
        .unwrap_err()
        .to_string()
        .contains("does not match"));
    let mut hidden = request;
    hidden.include_hidden = true;
    let page = discover(&client, &workspace, hidden).await.unwrap();
    assert!(page
        .entries
        .iter()
        .any(|entry| entry.label().ends_with("/.hidden")));
}

#[tokio::test]
async fn local_long_file_windows_are_bounded_and_reload_changed_content() {
    let directory = checkout();
    let path = directory.path().join("checkout/long");
    fs::write(
        &path,
        (1..=50_000)
            .map(|n| format!("line {n}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    let source = local(workspace.roots().remove(0), "long");
    for line in [1, 241, 15_000, 49_999, 90_000] {
        let content = workspace.read(source.clone(), line).await.unwrap();
        assert!(content.line_count <= 240);
        assert!(content.content.len() < 4096);
        assert_eq!(content.total_lines, 50_000);
        assert!(content
            .content
            .contains(&format!("line {}", line.min(50_000))));
    }
    fs::write(&path, "changed\n").unwrap();
    assert_eq!(workspace.read(source, 1).await.unwrap().content, "changed");
}

#[tokio::test]
async fn local_reads_reject_traversal_nontext_oversize_and_ungranted_roots() {
    let directory = checkout();
    fs::write(directory.path().join("outside"), "secret").unwrap();
    fs::write(directory.path().join("checkout/binary"), b"a\0b").unwrap();
    fs::write(
        directory.path().join("checkout/large"),
        vec![b'x'; 2 * 1024 * 1024 + 1],
    )
    .unwrap();
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    let root = workspace.roots().remove(0);
    for path in ["../outside", "/outside", "binary", "large", ""] {
        assert!(
            workspace.read(local(root.clone(), path), 1).await.is_err(),
            "{path}"
        );
    }
    let mut forged = root.clone();
    forged.name = "not-granted".into();
    assert!(workspace.read(local(forged, "file"), 1).await.is_err());
    let mut forged = root;
    forged.debugger_path = Some("/forged".into());
    assert!(workspace.read(local(forged, "file"), 1).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn local_symlink_escape_and_fifo_never_escape_or_block() {
    use std::os::unix::fs::symlink;
    let directory = checkout();
    fs::write(directory.path().join("outside"), "secret").unwrap();
    symlink("../outside", directory.path().join("checkout/escape")).unwrap();
    assert!(std::process::Command::new("mkfifo")
        .arg(directory.path().join("checkout/pipe"))
        .status()
        .unwrap()
        .success());
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    let root = workspace.roots().remove(0);
    for path in ["escape", "pipe"] {
        assert!(tokio::time::timeout(
            std::time::Duration::from_secs(1),
            workspace.read(local(root.clone(), path), 1)
        )
        .await
        .unwrap()
        .is_err());
    }
    assert!(Workspace::load(&directory.path().join("checkout/pipe")).is_err());
}

#[test]
fn local_history_and_directory_navigation_survive_backend_replacement() {
    let root = LocalRoot {
        name: "local".into(),
        debugger_path: Some("/build".into()),
        session_tags: Vec::new(),
    };
    let mut browser = SourceBrowser::default();
    browser.set_server_instance("test".into());
    for path in ["first", "second"] {
        let mut file = local(root.clone(), path);
        file.owner_hint.push(v2::SourceOwner {
            session_id: "old-owner".into(),
            ..Default::default()
        });
        file.owner_instance = Some("test".into());
        let source = Source::Local(file);
        let request = browser.open_source(source.clone());
        assert!(browser.apply(
            request.generation,
            Content {
                source,
                start_line: 1,
                content: "a\nb".into(),
                line_count: 2,
                total_lines: 2
            }
        ));
        browser.set_cursor(2);
    }
    let back = browser.history(false).unwrap();
    browser.invalidate_server();
    browser.set_server_instance("replacement".into());
    assert_eq!(browser.document().unwrap().cursor, 2);
    assert!(browser.document().unwrap().source.owners(&[]).is_empty());
    let Address::File(source) = back.address else {
        panic!("history file")
    };
    assert!(browser.apply(
        back.generation,
        Content {
            source,
            start_line: 1,
            content: "a\nb".into(),
            line_count: 2,
            total_lines: 2
        }
    ));
    assert_eq!(browser.cursor_location(), Some(("local/first", 2)));
    assert!(browser.document().unwrap().source.owners(&[]).is_empty());
    assert!(
        browser.history(false).is_none(),
        "pending local history must commit exactly once"
    );
    assert!(browser.history(true).is_some());
    let mut explorer = Explorer::default();
    let request = explorer.open(false);
    explorer.apply(
        request.generation,
        Page {
            entries: vec![Entry::Root(Root::Local(root))],
            next_token: None,
            warnings: Vec::new(),
        },
    );
    let Some(Action::Request(request)) = explorer.activate() else {
        panic!("root")
    };
    explorer.invalidate_server();
    explorer.set_server_instance("replacement".into());
    assert!(explorer.apply(
        request.generation,
        Page {
            entries: Vec::new(),
            next_token: None,
            warnings: Vec::new()
        }
    ));
    assert!(explorer.visible);
}

#[test]
fn local_tag_ownership_rebinds_to_new_session_ids_without_parsing_names() {
    let source = Source::Local(local(
        LocalRoot {
            name: "app".into(),
            debugger_path: Some("/build".into()),
            session_tags: vec!["alpha".into()],
        },
        "main.rs",
    ));
    assert_eq!(
        source.owners(&[session("old", "alpha")])[0].session_id,
        "old"
    );
    assert_eq!(
        source.owners(&[session("new", "alpha")])[0].session_id,
        "new"
    );
    assert!(source.owners(&[session("old", "beta")]).is_empty());
}

#[test]
fn response_context_must_identify_the_expected_backend() {
    assert!(instance(None, None).is_err());
    assert!(instance(Some(v2::ResponseContext::default()), None).is_err());
    assert!(instance(
        Some(v2::ResponseContext {
            server_instance_id: "new".into(),
            ..Default::default()
        }),
        Some("old")
    )
    .is_err());
}

// Real SDK calls exercise checks before a replacement state snapshot arrives.
async fn server(epoch: &str) -> (DdbClient, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};
    let count = Arc::new(AtomicUsize::new(0));
    let context = Some(v2::ResponseContext {
        server_instance_id: epoch.into(),
        ..Default::default()
    });
    let resolve_context = context.clone();
    let roots_context = context.clone();
    let catalog_context = context.clone();
    let entries_context = context.clone();
    let read_count = count.clone();
    let router = Router::new()
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListSourceRoots",
            post(move || async move {
                Json(v2::ListSourceRootsResponse {
                    context: roots_context,
                    ..Default::default()
                })
            }),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListSources",
            post(move || async move {
                Json(v2::ListSourcesResponse {
                    context: catalog_context,
                    ..Default::default()
                })
            }),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ListSourceEntries",
            post(move || async move {
                Json(v2::ListSourceEntriesResponse {
                    context: entries_context,
                    ..Default::default()
                })
            }),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ResolveSource",
            post(move || async move {
                Json(v2::ResolveSourceResponse {
                    context: resolve_context,
                    source: Some(v2::SourceFile {
                        source_reference: "ref".into(),
                        debugger_path: Some("/build/main.rs".into()),
                        ..Default::default()
                    }),
                })
            }),
        )
        .route(
            "/api/v2/rpc/ddb.api.v2.DebuggerService/ReadSource",
            post(move || async move {
                read_count.fetch_add(1, Ordering::SeqCst);
                Json(v2::ReadSourceResponse {
                    context,
                    source: Some(v2::SourceContent {
                        source: Some(v2::SourceFile {
                            source_reference: "ref".into(),
                            ..Default::default()
                        }),
                        content: "backend".into(),
                        start_line: 1,
                        line_count: 1,
                        total_lines: 1,
                        has_more: false,
                    }),
                })
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = DdbClient::new(ClientConfig::new(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (client, count, server)
}

#[tokio::test]
async fn reads_and_resolves_reject_a_replacement_backend_before_any_snapshot() {
    let (client, reads, server) = server("new").await;
    let workspace = Workspace::default();
    let mut browser = SourceBrowser::default();
    browser.set_server_instance("test".into());
    let request = browser
        .navigate(
            None,
            v2::SourceLocation {
                path: Some("/build/main.rs".into()),
                ..Default::default()
            },
            crate::source_browser::Navigation::Manual,
        )
        .unwrap();
    assert!(read(&client, &workspace, request)
        .await
        .unwrap_err()
        .to_string()
        .contains("instance changed"));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let request = browser.open_source(source("/build/main.rs", &[]));
    assert!(read(&client, &workspace, request)
        .await
        .unwrap_err()
        .to_string()
        .contains("instance changed"));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn explicit_mapping_reads_local_content_and_never_falls_back_on_local_error() {
    let directory = checkout();
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    let (client, reads, server) = server("test").await;
    let mut browser = SourceBrowser::default();
    let request = browser.open_source(source("/build/main.rs", &[]));
    assert!(read(&client, &workspace, request).await.is_err());
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "missing mapped file must not fall back to backend"
    );
    fs::write(directory.path().join("checkout/main.rs"), "local").unwrap();
    let request = browser
        .navigate(
            None,
            v2::SourceLocation {
                path: Some("/build/main.rs".into()),
                ..Default::default()
            },
            crate::source_browser::Navigation::Manual,
        )
        .unwrap();
    assert_eq!(
        read(&client, &workspace, request).await.unwrap().content,
        "local"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let request = browser.open_source(source("/unmapped/main.rs", &[]));
    assert_eq!(
        read(&client, &workspace, request).await.unwrap().content,
        "backend"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn every_remote_discovery_operation_checks_its_instance() {
    let (client, _, server) = server("new").await;
    let workspace = Workspace::default();
    let mut explorer = Explorer::default();
    explorer.set_server_instance("old".into());
    for catalog in [false, true] {
        let request = explorer.open(catalog);
        assert!(discover(&client, &workspace, request)
            .await
            .unwrap_err()
            .to_string()
            .contains("instance changed"));
    }
    let mut request = explorer.open(false);
    request.place = Place::Directory {
        root: Root::Ddb {
            instance: "old".into(),
            root: v2::SourceRoot {
                root_reference: "root".into(),
                ..Default::default()
            },
        },
        path: String::new(),
    };
    assert!(discover(&client, &workspace, request)
        .await
        .unwrap_err()
        .to_string()
        .contains("instance changed"));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn wrong_source_reference_cannot_replace_the_requested_file() {
    let (client, _, server) = server("test").await;
    let mut file = source("/build/main.rs", &[]);
    let Source::Ddb { file: remote, .. } = &mut file else {
        unreachable!()
    };
    remote.source_reference = "another-file".into();
    let request = SourceBrowser::default().open_source(file);
    assert!(read(&client, &Workspace::default(), request)
        .await
        .unwrap_err()
        .to_string()
        .contains("different source identity"));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn stalled_remote_roots_do_not_block_local_roots_past_the_discovery_budget() {
    use axum::{routing::post, Router};
    let directory = checkout();
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    let router = Router::new().route(
        "/api/v2/rpc/ddb.api.v2.DebuggerService/ListSourceRoots",
        post(|| async { std::future::pending::<()>().await }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = DdbClient::new(ClientConfig::new(format!(
        "http://{}",
        listener.local_addr().unwrap()
    )))
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let request = Explorer::default().open(false);
    let page = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        discover(&client, &workspace, request),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(&page.entries[..], [Entry::Root(Root::Local(_))]));
    assert!(page.warnings[0].contains("timed out"));
    server.abort();
    let _ = server.await;
}

#[cfg(unix)]
#[tokio::test]
async fn cached_local_reads_and_directory_pages_recheck_revoked_permissions() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    } // Root bypasses ordinary DAC checks.
    let directory = checkout();
    let folder = directory.path().join("checkout/private");
    fs::create_dir(&folder).unwrap();
    let file = folder.join("text");
    fs::write(&file, "private").unwrap();
    let workspace = config(&directory, json!([root("app", "/build", &[])])).unwrap();
    let root = workspace.roots().remove(0);
    workspace
        .read(local(root.clone(), "private/text"), 1)
        .await
        .unwrap();
    workspace
        .list(root.clone(), "private".into(), false, false)
        .await
        .unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
    let denied = workspace.read(local(root.clone(), "private/text"), 1).await;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(denied.is_err());
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o0)).unwrap();
    let denied = workspace.list(root, "private".into(), false, true).await;
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(denied.is_err());
}
