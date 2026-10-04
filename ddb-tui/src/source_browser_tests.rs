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
        total_lines: 4,
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
fn cursor_crosses_read_windows_without_creating_history_and_retries_after_failure() {
    let mut browser = SourceBrowser::default();
    let mut first = content("large.rs");
    first.total_lines = 1000;
    first.has_more = true;
    show_source(&mut browser, first, 4);
    let first = browser.move_cursor(1).unwrap();
    assert_eq!(first.line, 5);
    let second = browser.move_cursor(10).unwrap();
    assert_eq!(second.line, 15);
    assert!(!browser.apply(first.generation, content("large.rs")));
    assert!(browser.fail(second.generation));
    assert_eq!(browser.document().unwrap().cursor, 4);
    let retry = browser.move_cursor(1).unwrap();
    let mut window = content("large.rs");
    window.start_line = 5;
    window.total_lines = 1000;
    assert!(browser.apply(retry.generation, window));
    assert_eq!(browser.document().unwrap().cursor, 5);
    assert!(browser.history(false).is_none());
    let previous = browser.move_cursor(-1).unwrap();
    assert_eq!(previous.line, 4);
    assert!(!browser.following());
}

#[test]
fn reopening_a_document_restores_its_cursor_by_opaque_identity() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "first.rs", 3);
    open(&mut browser, "second.rs", 2);
    let first = content("first.rs");
    let request = browser.open_source(first.source.clone().unwrap());
    assert_eq!(request.line, 3);
    assert!(browser.apply(request.generation, first));
    assert_eq!(browser.document().unwrap().cursor, 3);
    let mut other = content("first.rs").source.unwrap();
    other.source_reference = "different-root/first.rs".into();
    let request = browser.open_source(other);
    assert_eq!(
        request.line, 1,
        "equal display paths cannot share cursor state"
    );
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
    let mut content_a = content("main.rs");
    content_a.source.as_mut().unwrap().source_reference = "a/main.rs".into();
    let mut content_b = content("main.rs");
    content_b.source.as_mut().unwrap().source_reference = "b/main.rs".into();
    let a = browser
        .navigate(owner_a, location("main.rs", 2), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(a.generation, content_a.clone()));
    let b = browser
        .navigate(owner_b, location("main.rs", 2), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(b.generation, content_b));
    assert_eq!(
        browser.document().unwrap().source.source_reference,
        "b/main.rs"
    );
    let back = browser.history(false).unwrap();
    assert!(back.target.is_none());
    assert_eq!(back.location.source_reference.as_deref(), Some("a/main.rs"));
    assert!(browser.apply(back.generation, content_a));
    assert_eq!(
        browser.document().unwrap().source.source_reference,
        "a/main.rs"
    );
    assert_eq!(browser.document().unwrap().cursor, 2);
}

#[test]
fn reopening_a_frame_document_does_not_add_history_for_a_target_change() {
    let mut browser = SourceBrowser::default();
    let request = browser
        .navigate(
            Some(crate::api::thread_target("thread")),
            location("main.rs", 2),
            Navigation::Frame,
        )
        .unwrap();
    assert!(browser.apply(request.generation, content("main.rs")));
    let request = browser.open_source(content("main.rs").source.unwrap());
    assert!(browser.apply(request.generation, content("main.rs")));
    assert!(browser.history(false).is_none());
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
        source.total_lines = count;
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
