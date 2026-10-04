use super::*;

fn file(name: &str, owner: &str) -> Entry {
    Entry::Source(v2::SourceFile {
        source_reference: format!("{owner}/{name}"),
        path: Some(name.into()),
        owners: vec![v2::SourceOwner {
            session_id: owner.into(),
            ..Default::default()
        }],
        ..Default::default()
    })
}

fn page(entries: Vec<Entry>, next: Option<&str>) -> Page {
    Page {
        entries,
        next_token: next.map(str::to_owned),
        warnings: Vec::new(),
    }
}

#[test]
fn catalog_retains_distinct_owners_of_identical_paths() {
    let mut explorer = Explorer::default();
    let request = explorer.open(true);
    assert!(explorer.apply(
        request.generation,
        page(vec![file("main.rs", "a"), file("main.rs", "b")], None)
    ));
    explorer.select(1);
    let Some(Action::Open(source)) = explorer.activate() else {
        panic!("source should open")
    };
    assert_eq!(source.source_reference, "b/main.rs");
    assert_eq!(source.owners[0].session_id, "b");
}

#[test]
fn stale_queries_and_closed_dialogs_reject_results_and_errors() {
    let mut explorer = Explorer::default();
    let old = explorer.open(true);
    let next = explorer.edit_query(Some("main")).unwrap();
    assert!(!explorer.apply(old.generation, page(vec![file("old.rs", "a")], None)));
    assert!(!explorer.fail(old.generation, "old failure".into()));
    explorer.close();
    assert!(!explorer.apply(next.generation, page(Vec::new(), None)));
    assert!(!explorer.fail(next.generation, "late failure".into()));
}

#[test]
fn directory_navigation_is_lazy_and_parent_returns_to_roots() {
    let mut explorer = Explorer::default();
    let request = explorer.open(false);
    explorer.apply(
        request.generation,
        page(
            vec![Entry::Root(v2::SourceRoot {
                root_reference: "root-a".into(),
                name: "project".into(),
                ..Default::default()
            })],
            None,
        ),
    );
    let Some(Action::Request(request)) = explorer.activate() else {
        panic!("root should enumerate on demand")
    };
    assert!(matches!(&request.place, Place::Directory { path, .. } if path.is_empty()));
    explorer.apply(
        request.generation,
        page(
            vec![Entry::Directory(v2::SourceEntry {
                name: "src".into(),
                relative_path: "src".into(),
                ..Default::default()
            })],
            None,
        ),
    );
    let Some(Action::Request(request)) = explorer.activate() else {
        panic!("directory should enumerate on demand")
    };
    assert!(matches!(&request.place, Place::Directory { path, .. } if path == "src"));
    explorer.parent().unwrap();
    assert!(matches!(&explorer.place, Place::Directory { path, .. } if path.is_empty()));
    explorer.parent().unwrap();
    assert_eq!(explorer.place, Place::Roots);
}

#[test]
fn page_navigation_replaces_entries_and_failed_reads_preserve_page_history() {
    let mut explorer = Explorer::default();
    let request = explorer.open(true);
    explorer.apply(
        request.generation,
        page(vec![file("first", "a")], Some("page-2")),
    );
    explorer.select(1);
    let Some(Action::Request(next)) = explorer.activate() else {
        panic!("next page request")
    };
    assert_eq!(next.page_token.as_deref(), Some("page-2"));
    explorer.fail(next.generation, "temporary failure".into());
    assert!(!explorer.has_previous());
    let Some(Action::Request(next)) = explorer.activate() else {
        panic!("retry next page")
    };
    explorer.apply(next.generation, page(vec![file("second", "a")], None));
    assert_eq!(explorer.entries.len(), 1);
    assert_eq!(explorer.entries[0].label(), "second");
    assert!(explorer.has_previous());
    explorer.select(0);
    let Some(Action::Request(previous)) = explorer.activate() else {
        panic!("previous page request")
    };
    assert!(previous.page_token.is_none());
    explorer.apply(
        previous.generation,
        page(vec![file("first", "a")], Some("page-2")),
    );
    assert!(!explorer.has_previous());
}

#[test]
fn refresh_starts_from_first_page_and_keeps_selected_identity() {
    let mut explorer = Explorer::default();
    let request = explorer.open(true);
    explorer.apply(
        request.generation,
        page(vec![file("a", "owner"), file("b", "owner")], None),
    );
    explorer.select(1);
    let request = explorer.refresh();
    assert!(request.page_token.is_none());
    assert!(request.refresh_catalog);
    explorer.apply(
        request.generation,
        page(vec![file("b", "owner"), file("a", "owner")], None),
    );
    assert_eq!(explorer.selected, 0);
}

#[test]
fn query_editing_is_unicode_safe_bounded_and_suppresses_terminal_controls() {
    let mut explorer = Explorer::default();
    explorer.open(true);
    explorer.edit_query(Some("目录\u{1b}\n"));
    assert_eq!(explorer.query, "目录");
    explorer.edit_query(None);
    assert_eq!(explorer.query, "目");
    explorer.edit_query(Some(&"a".repeat(2000)));
    assert!(explorer.query.len() <= 1024);
    explorer.clear_query();
    assert!(explorer.query.is_empty());
}

#[test]
fn invalid_pages_release_loading_without_committing_page_history() {
    let mut explorer = Explorer::default();
    let request = explorer.open(true);
    explorer.apply(
        request.generation,
        page(vec![file("first", "a")], Some("next")),
    );
    explorer.select(1);
    let Some(Action::Request(request)) = explorer.activate() else {
        panic!("next page")
    };
    explorer.apply(
        request.generation,
        page(vec![file("bad", "a")], Some("next")),
    );
    assert!(!explorer.loading());
    assert!(explorer
        .error
        .as_ref()
        .unwrap()
        .contains("invalid source page"));
    assert!(!explorer.has_previous());
    assert_eq!(explorer.entries[0].label(), "first");
    let request = explorer.refresh();
    explorer.apply(
        request.generation,
        page(
            (0..=PAGE_SIZE).map(|_| file("too many", "a")).collect(),
            None,
        ),
    );
    assert_eq!(explorer.entries.len(), 1);
    assert!(!explorer.loading());
}

#[test]
fn symlinks_have_explicit_file_and_directory_actions() {
    let mut explorer = Explorer {
        visible: true,
        place: Place::Directory {
            root: v2::SourceRoot {
                root_reference: "root".into(),
                ..Default::default()
            },
            path: String::new(),
        },
        ..Default::default()
    };
    explorer.entries.push(Entry::Link(v2::SourceEntry {
        name: "link".into(),
        relative_path: "link".into(),
        ..Default::default()
    }));
    assert!(
        matches!(explorer.activate(), Some(Action::Resolve { root_reference, path }) if root_reference == "root" && path == "link")
    );
    let Some(Action::Request(request)) = explorer.enter_directory() else {
        panic!("directory request")
    };
    assert!(matches!(request.place, Place::Directory { path, .. } if path == "link"));
    explorer.fail(request.generation, "escaped or broken link".into());
    assert!(explorer.error.is_some());
    assert!(explorer.parent().is_some());
}
