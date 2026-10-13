use super::*;

pub(crate) fn show_source(browser: &mut SourceBrowser, content: v2::SourceContent, line: usize) {
    show_content(browser, remote_content(content), line);
}

pub(crate) fn remote_source(file: v2::SourceFile) -> Source {
    Source::Ddb {
        instance: "test".into(),
        file,
    }
}

pub(crate) fn remote_content(content: v2::SourceContent) -> Content {
    Content::from_ddb("test".into(), content).unwrap()
}

fn show_content(browser: &mut SourceBrowser, content: Content, line: usize) {
    browser.set_server_instance("test".into());
    let location = v2::SourceLocation {
        path: Some(content.source.path().into_owned()),
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

fn content(path: &str) -> Content {
    Content {
        source: remote_source(v2::SourceFile {
            source_reference: format!("source/{path}"),
            path: Some(path.into()),
            name: path.into(),
            ..Default::default()
        }),
        start_line: 1,
        content: "one\ntwo\nthree\nfour".into(),
        line_count: 4,
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
fn horizontal_scroll_preserves_cursor_following_and_file_history() {
    let mut browser = SourceBrowser::default();
    let mut long = content("wide.rs");
    long.content = format!("{}\nsecond\nthird\nfourth", "abcdef".repeat(20));
    let request = browser
        .navigate(None, location("wide.rs", 1), Navigation::Follow)
        .unwrap();
    assert!(browser.apply(request.generation, long.clone()));
    browser.fit_horizontal(20);
    browser.scroll_horizontal(8);
    assert_eq!(browser.document().unwrap().column, 8);
    assert_eq!(browser.cursor_location(), Some(("wide.rs", 1)));
    assert!(browser.following());
    assert!(browser.pending_generation().is_none());
    open(&mut browser, "other.rs", 1);
    assert_eq!(browser.document().unwrap().column, 0);
    let request = browser.history(false).unwrap();
    assert!(browser.apply(request.generation, long.clone()));
    assert_eq!(browser.document().unwrap().column, 8);
    let request = browser.go_to_line(3).unwrap();
    assert!(browser.apply(request.generation, long));
    assert_eq!(browser.document().unwrap().column, 8);
    browser.scroll_horizontal(isize::MAX);
    assert_eq!(browser.document().unwrap().column, 100);
    browser.fit_horizontal(110);
    assert_eq!(browser.document().unwrap().column, 10);
    browser.scroll_horizontal(-100);
    assert_eq!(browser.document().unwrap().column, 0);
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
fn crossing_source_windows_keeps_cursor_at_the_scrolling_edge() {
    fn window(first: u32) -> Content {
        let mut data = content("large.rs");
        data.start_line = first;
        data.line_count = 100;
        data.total_lines = 1000;
        data.content = (first..first + 100)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        data
    }
    let mut browser = SourceBrowser::default();
    let request = browser
        .navigate(None, location("large.rs", 100), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(request.generation, window(1)));
    browser.fit_viewport(20);
    let request = browser.move_cursor(1).unwrap();
    assert!(browser.apply(request.generation, window(81)));
    browser.fit_viewport(20);
    let document = browser.document().unwrap();
    assert_eq!(document.cursor - document.start_line - document.scroll, 19);

    browser.set_cursor(81);
    browser.fit_viewport(20);
    let request = browser.move_cursor(-1).unwrap();
    assert!(browser.apply(request.generation, window(61)));
    browser.fit_viewport(20);
    let document = browser.document().unwrap();
    assert_eq!(document.cursor - document.start_line - document.scroll, 0);
    browser.set_cursor(160);
    browser.fit_viewport(20);
    let obsolete = browser.move_cursor(1).unwrap();
    let latest = browser.move_cursor(1).unwrap();
    browser.fit_viewport(10);
    assert!(!browser.apply(obsolete.generation, window(141)));
    assert!(browser.apply(latest.generation, window(141)));
    browser.fit_viewport(10);
    let document = browser.document().unwrap();
    assert_eq!(document.cursor, 162);
    assert_eq!(document.cursor - document.start_line - document.scroll, 9);
}

#[test]
fn cursor_crosses_read_windows_without_creating_history_and_retries_after_failure() {
    let mut browser = SourceBrowser::default();
    let mut first = content("large.rs");
    first.total_lines = 1000;
    show_content(&mut browser, first, 4);
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
fn moving_cursor_within_visible_page_does_not_scroll_document() {
    let mut browser = SourceBrowser::default();
    let mut file = content("large.rs");
    file.content = (1..=100)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    file.line_count = 100;
    file.total_lines = 100;
    show_content(&mut browser, file, 1);
    // Cursor movement within a page must not pin the cursor to row nine.
    for _ in 0..15 {
        assert!(browser.move_cursor(1).is_none());
        browser.fit_viewport(20);
    }
    assert_eq!(browser.document().unwrap().cursor, 16);
    assert_eq!(browser.document().unwrap().scroll, 0);
    browser.move_cursor(5);
    browser.fit_viewport(20);
    assert_eq!(browser.document().unwrap().scroll, 1);
    browser.move_cursor(-1);
    browser.fit_viewport(20);
    assert_eq!(browser.document().unwrap().scroll, 1);
    browser.fit_viewport(5);
    assert_eq!(browser.document().unwrap().scroll, 15);
    browser.move_cursor(-10);
    browser.fit_viewport(5);
    assert_eq!(browser.document().unwrap().scroll, 9);
    browser.fit_viewport(200);
    assert_eq!(browser.document().unwrap().scroll, 0);
}

#[test]
fn reopening_a_document_restores_its_cursor_by_opaque_identity() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "first.rs", 3);
    open(&mut browser, "second.rs", 2);
    let first = content("first.rs");
    let request = browser.open_source(first.source.clone());
    assert_eq!(request.line, 3);
    assert!(browser.apply(request.generation, first));
    assert_eq!(browser.document().unwrap().cursor, 3);
    let mut other = content("first.rs").source;
    let Source::Ddb { file, .. } = &mut other else {
        unreachable!()
    };
    file.source_reference = "different-root/first.rs".into();
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
    show_content(&mut browser, content("main.rs"), 1);
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
    assert_eq!(back.address.id(), Some(content("main.rs").source.id()));
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
fn source_errors_hide_stale_document_and_new_navigation_recovers() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "old.rs", 2);
    let missing = browser
        .navigate(None, location("missing.rs", 3), Navigation::Frame)
        .unwrap();
    assert!(browser.fail_with_error(missing.generation, "missing.rs: permission denied".into()));
    assert!(
        browser.document().is_none(),
        "stale source must not be actionable behind an error"
    );
    assert!(browser.error().unwrap().contains("permission denied"));
    assert!(browser.move_cursor(1).is_none());
    let newer = browser
        .navigate(None, location("new.rs", 1), Navigation::Frame)
        .unwrap();
    assert!(!browser.fail_with_error(missing.generation, "stale failure".into()));
    assert!(browser.error().is_none());
    assert!(browser.apply(newer.generation, content("new.rs")));
    assert_eq!(browser.document().unwrap().path, "new.rs");
    let back = browser.history(false).unwrap();
    assert_eq!(
        back.line, 2,
        "failed navigation must preserve the prior document's position"
    );
}

#[test]
fn same_path_with_different_owners_remains_distinct_in_history() {
    let mut browser = SourceBrowser::default();
    let owner_a = Some(crate::api::session_target("a"));
    let owner_b = Some(crate::api::session_target("b"));
    let mut content_a = content("main.rs");
    let Source::Ddb { file, .. } = &mut content_a.source else {
        unreachable!()
    };
    file.source_reference = "a/main.rs".into();
    let mut content_b = content("main.rs");
    let Source::Ddb { file, .. } = &mut content_b.source else {
        unreachable!()
    };
    file.source_reference = "b/main.rs".into();
    let id_b = content_b.source.id();
    let a = browser
        .navigate(owner_a, location("main.rs", 2), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(a.generation, content_a.clone()));
    let b = browser
        .navigate(owner_b, location("main.rs", 2), Navigation::Manual)
        .unwrap();
    assert!(browser.apply(b.generation, content_b));
    assert_eq!(browser.document().unwrap().source.id(), id_b);
    let back = browser.history(false).unwrap();
    assert_eq!(back.address.id(), Some(content_a.source.id()));
    let id_a = content_a.source.id();
    assert!(browser.apply(back.generation, content_a));
    assert_eq!(browser.document().unwrap().source.id(), id_a);
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
    let request = browser.open_source(content("main.rs").source);
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
        show_content(&mut browser, source, 1);
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

#[test]
fn initial_source_lookup_cannot_overwrite_a_different_instance_snapshot() {
    let mut browser = SourceBrowser::default();
    let request = browser
        .navigate(None, location("main.rs", 1), Navigation::Manual)
        .unwrap();
    browser.set_server_instance("new".into());
    assert!(!browser.apply(request.generation, content("main.rs")));
    assert!(browser.document().is_none());
    assert!(browser.pending_generation().is_none());
}

#[test]
fn first_snapshot_discards_a_document_loaded_from_an_earlier_instance() {
    let mut browser = SourceBrowser::default();
    open(&mut browser, "main.rs", 2);
    browser.set_server_instance("new".into());
    assert!(browser.document().is_none());
    assert!(browser.history(false).is_none());
}
