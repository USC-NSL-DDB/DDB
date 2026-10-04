use super::*;

pub(crate) fn show_source(browser: &mut SourceBrowser, content: v2::SourceContent, line: usize) {
    let location = v2::SourceLocation {
        path: content
            .source
            .as_ref()
            .and_then(|source| source.path.clone()),
        line: line as u32,
        ..Default::default()
    };
    if let Some(request) = browser.navigate(None, location, Navigation::Follow) {
        assert!(browser.apply(request.generation, content));
    }
}

fn location(path: &str, line: u32) -> v2::SourceLocation {
    v2::SourceLocation {
        path: Some(path.into()),
        line,
        ..Default::default()
    }
}

fn content(path: &str) -> v2::SourceContent {
    v2::SourceContent {
        source: Some(v2::SourceFile {
            source_reference: format!("source/{path}"),
            path: Some(path.into()),
            name: path.into(),
            ..Default::default()
        }),
        start_line: 1,
        content: "one\ntwo\nthree\nfour".into(),
        line_count: 4,
        has_more: false,
    }
}

fn open(browser: &mut SourceBrowser, path: &str, line: u32) {
    let request = browser
        .navigate(None, location(path, line), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(request.generation, content(path)));
}

#[test]
fn manual_open_needs_no_thread_and_disables_following() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "main.rs", 2);
    assert_eq!(browser.cursor_location(), Some(("main.rs", 2)));
    assert!(!browser.following());
    assert!(browser
        .navigate(None, location("worker.rs", 1), Navigation::Follow)
        .is_none());
    assert_eq!(browser.cursor_location(), Some(("main.rs", 2)));
}

#[test]
fn explicit_follow_resumes_at_the_requested_stop() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "main.rs", 2);
    browser.set_following(true);
    let request = browser
        .navigate(None, location("worker.rs", 3), Navigation::Follow)
        .unwrap();
    assert!(browser.apply(request.generation, content("worker.rs")));
    assert_eq!(browser.cursor_location(), Some(("worker.rs", 3)));
    assert!(browser.following());
}

#[test]
fn stale_successes_and_errors_do_not_replace_newer_navigation() {
    let mut browser = SourceBrowser::default();
    let old = browser
        .navigate(None, location("old.rs", 1), Navigation::Manual)
        .unwrap();
    let new = browser
        .navigate(None, location("new.rs", 3), Navigation::Manual)
        .unwrap();
    assert!(!browser.apply(old.generation, content("old.rs")));
    assert!(!browser.fail(old.generation));
    assert!(browser.apply(new.generation, content("new.rs")));
    assert_eq!(browser.cursor_location(), Some(("new.rs", 3)));
}

#[test]
fn moving_the_cursor_cancels_a_pending_follow_response() {
    let mut browser = SourceBrowser::default();
    show_source(&mut browser, content("main.rs"), 1);
    let request = browser
        .navigate(None, location("worker.rs", 2), Navigation::Follow)
        .unwrap();
    browser.set_cursor(4);
    assert!(!browser.apply(request.generation, content("worker.rs")));
    assert_eq!(browser.cursor_location(), Some(("main.rs", 4)));
}

#[test]
fn history_restores_document_positions_and_can_go_forward() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "main.rs", 2);
    browser.set_cursor(4);
    open(&mut browser, "worker.rs", 3);
    let back = browser.history(false).unwrap();
    assert_eq!(
        back.location.source_reference.as_deref(),
        Some("source/main.rs")
    );
    assert!(browser.apply(back.generation, content("main.rs")));
    assert_eq!(browser.cursor_location(), Some(("main.rs", 4)));
    let forward = browser.history(true).unwrap();
    assert!(browser.apply(forward.generation, content("worker.rs")));
    assert_eq!(browser.cursor_location(), Some(("worker.rs", 3)));
}

#[test]
fn failed_navigation_does_not_corrupt_history() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "main.rs", 2);
    let failed = browser
        .navigate(None, location("missing.rs", 1), Navigation::Manual)
        .unwrap();
    assert!(browser.fail(failed.generation));
    assert!(browser.history(false).is_none());
    open(&mut browser, "worker.rs", 3);
    let back = browser.history(false).unwrap();
    assert!(browser.fail(back.generation));
    let retry = browser.history(false).unwrap();
    assert!(browser.apply(retry.generation, content("main.rs")));
    assert_eq!(browser.cursor_location(), Some(("main.rs", 2)));
}

#[test]
fn same_path_with_different_owners_remains_distinct_in_history() {
    let mut browser = SourceBrowser::default();
    let owner_a = Some(crate::api::session_target("a"));
    let owner_b = Some(crate::api::session_target("b"));
    let a = browser
        .navigate(owner_a.clone(), location("main.rs", 2), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(a.generation, content("main.rs")));
    let b = browser
        .navigate(owner_b.clone(), location("main.rs", 3), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(b.generation, content("main.rs")));
    assert_eq!(browser.document().unwrap().target, owner_b);
    let back = browser.history(false).unwrap();
    assert_eq!(back.target, owner_a);
    assert!(browser.apply(back.generation, content("main.rs")));
    assert_eq!(browser.document().unwrap().target, owner_a);
    assert_eq!(browser.document().unwrap().cursor, 2);
}

#[test]
fn source_content_preserves_empty_and_blank_lines() {
    for (text, count, expected) in [
        ("", 0, vec![]),
        ("", 1, vec![""]),
        ("first\n", 2, vec!["first", ""]),
    ] {
        let mut browser = SourceBrowser::default();
        let mut source = content("main.rs");
        source.content = text.into();
        source.line_count = count;
        show_source(&mut browser, source, 1);
        assert_eq!(browser.document().unwrap().lines, expected);
    }
}

#[test]
fn selecting_a_frame_reveals_it_without_enabling_follow() {
    let mut browser = SourceBrowser::default();
    let request = browser
        .navigate(None, location("caller.rs", 3), Navigation::Frame)
        .unwrap();
    assert!(browser.apply(request.generation, content("caller.rs")));
    assert_eq!(browser.cursor_location(), Some(("caller.rs", 3)));
    assert!(!browser.following());
}
