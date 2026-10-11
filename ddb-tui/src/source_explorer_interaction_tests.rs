use super::*;
use crate::source_browser::tests::remote_source;
use crate::source_explorer::{Control as ExplorerControl, Entry, Page, Place};
use crate::source_types::SourceId;
use ratatui::{backend::TestBackend, Terminal};

fn source_page() -> Page {
    Page {
        entries: vec![Entry::Source(remote_source(v2::SourceFile {
            source_reference: "source/other-session/main".into(),
            name: "main.rs".into(),
            path: Some("/build/main.rs".into()),
            owners: vec![v2::SourceOwner {
                session_id: "session/other".into(),
                ..Default::default()
            }],
            availability: v2::SourceAvailability::Unavailable as i32,
            unavailable_reason: Some("checkout is not mounted".into()),
            ..Default::default()
        }))],
        next_token: None,
        warnings: vec!["catalog refresh skipped for running session".into()],
    }
}

fn key(app: &mut App, sender: &mpsc::Sender<BackendRequest>, code: KeyCode) {
    handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), sender);
}

#[test]
fn source_text_search_keyboard_mouse_and_overload_states_are_discoverable() {
    let mut app = App::new("http://localhost".into());
    let source = remote_source(v2::SourceFile {
        source_reference: "file/search".into(),
        ..Default::default()
    });
    let request = app.source_browser.open_source(source.clone());
    assert!(app.source_browser.apply(
        request.generation,
        crate::source_types::Content {
            source,
            content: "let prefix = hello;".into(),
            start_line: 1,
            line_count: 1,
            total_lines: 1000,
        }
    ));
    app.focus = Focus::Source;
    let (sender, mut receiver) = mpsc::channel(8);
    key(&mut app, &sender, KeyCode::Char('/'));
    assert_eq!(app.input_mode, InputMode::SourceSearch);
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::CancelSourceSearch)
    ));
    app.insert_input(" prefix ");
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        &sender,
    );
    assert_eq!(app.source_search.mode, crate::source_search::Mode::Fuzzy);
    key(&mut app, &sender, KeyCode::Enter);
    assert_eq!(app.input_mode, InputMode::Normal);
    let Ok(BackendRequest::SearchSource(request)) = receiver.try_recv() else {
        panic!("missing search")
    };
    assert_eq!(request.query, " prefix ");
    assert!(request.include_origin);
    key(&mut app, &sender, KeyCode::Esc);
    assert!(!app.source_search.pending());
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::CancelSourceSearch)
    ));
    key(&mut app, &sender, KeyCode::Char('N'));
    let Ok(BackendRequest::SearchSource(request)) = receiver.try_recv() else {
        panic!("missing repeat")
    };
    assert!(request.backward && !request.include_origin);
    key(&mut app, &sender, KeyCode::Esc);
    receiver.try_recv().unwrap();
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| app.areas = ui::draw(frame, &mut app))
        .unwrap();
    let mode = app.areas.source_search_mode.unwrap();
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: mode.x,
            row: mode.y,
            modifiers: KeyModifiers::NONE,
        },
        &sender,
    );
    assert_eq!(app.source_search.mode, crate::source_search::Mode::Exact);
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::SearchSource(_))
    ));
    // A full worker queue must clear the spinner and show a useful error.
    let (full, _blocked) = mpsc::channel(1);
    full.try_send(BackendRequest::CancelSourceSearch).unwrap();
    find_source(&mut app, &full, Some("x".into()), false);
    assert!(!app.source_search.pending());
    assert!(app.status.contains("queue"));
    terminal
        .draw(|frame| app.areas = ui::draw(frame, &mut app))
        .unwrap();
    let panel = app
        .areas
        .panels
        .iter()
        .find(|panel| panel.focus == Focus::Source)
        .unwrap();
    let bar = app.areas.source_search_bar.unwrap();
    let visible_lines = bar.y - panel.area.y - 1;
    assert_eq!(panel.area.height - 2, visible_lines);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &sender,
    );
    let Ok(BackendRequest::LoadSource(request)) = receiver.try_recv() else {
        panic!("missing half-page source request")
    };
    assert_eq!(request.line, 1 + visible_lines as usize / 2);
}

#[test]
fn direct_pane_shortcuts_are_stable_and_do_not_steal_prompt_input() {
    let mut app = App::new("http://localhost".into());
    let (sender, mut receiver) = mpsc::channel(4);
    for (number, focus) in [
        ('1', Focus::Threads),
        ('2', Focus::Breakpoints),
        ('3', Focus::Source),
        ('4', Focus::Stack),
        ('5', Focus::Variables),
        ('8', Focus::Timeline),
        ('9', Focus::Output),
    ] {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(number), KeyModifiers::ALT),
            &sender,
        );
        assert_eq!(app.focus, focus);
        assert_eq!(Focus::from_shortcut(number), Some(focus));
    }
    assert!(app.show_output && app.show_timeline);
    key(&mut app, &sender, KeyCode::Char('z'));
    assert!(app.expanded_pane);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT),
        &sender,
    );
    assert!(app.expanded_pane);
    assert_eq!(app.focus, Focus::Source);
    key(&mut app, &sender, KeyCode::Esc);
    assert!(!app.expanded_pane);
    app.input_mode = InputMode::Evaluate;
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT),
        &sender,
    );
    assert_eq!(app.focus, Focus::Source);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn half_page_source_navigation_loads_missing_windows_and_never_refreshes_the_stack() {
    let mut app = App::new("http://localhost".into());
    let source = remote_source(v2::SourceFile {
        source_reference: "file/one".into(),
        ..Default::default()
    });
    let request = app.source_browser.open_source(source);
    app.source_browser.apply(
        request.generation,
        crate::source_browser::tests::remote_content(v2::SourceContent {
            source: Some(v2::SourceFile {
                source_reference: "file/one".into(),
                ..Default::default()
            }),
            start_line: 1,
            line_count: 3,
            total_lines: 100,
            content: "one\ntwo\nthree".into(),
            has_more: true,
        }),
    );
    app.areas.add_panel(
        Focus::Source,
        ratatui::layout::Rect::new(0, 0, 80, 22),
        0,
        1,
    );
    let (sender, mut receiver) = mpsc::channel(4);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &sender,
    );
    let BackendRequest::LoadSource(request) = receiver.try_recv().unwrap() else {
        panic!("expected another source window")
    };
    assert_eq!(request.line, 11);
    app.focus = Focus::Stack;
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        &sender,
    );
    assert!(
        receiver.try_recv().is_err(),
        "Ctrl+d must not invoke plain d"
    );
}

#[test]
fn searchable_help_keeps_navigation_keys_as_text_while_editing() {
    let mut app = App::new("http://localhost".into());
    let (sender, mut receiver) = mpsc::channel(4);
    app.show_help = true;
    key(&mut app, &sender, KeyCode::Char('/'));
    for character in "jump".chars() {
        key(&mut app, &sender, KeyCode::Char(character));
    }
    assert_eq!(app.help_query, "jump");
    assert_eq!(app.help_scroll, 0);
    key(&mut app, &sender, KeyCode::Enter);
    assert!(!app.help_searching);
    assert!(app.show_help);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| {
            ui::draw(frame, &mut app);
        })
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains("owning frame"));
    assert!(!screen.contains("Toggle Timeline"));
    key(&mut app, &sender, KeyCode::Esc);
    assert!(!app.show_help);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn explorer_and_goto_shortcuts_work_without_debugger_selection() {
    let mut app = App::new("http://localhost".into());
    let (sender, mut receiver) = mpsc::channel(4);
    key(&mut app, &sender, KeyCode::Char('g'));
    assert_eq!(app.input_mode, InputMode::GotoLine);
    key(&mut app, &sender, KeyCode::Esc);
    key(&mut app, &sender, KeyCode::Char('o'));
    let BackendRequest::ExploreSources(request) = receiver.try_recv().unwrap() else {
        panic!("roots should be requested without a thread");
    };
    assert_eq!(request.place, Place::Roots);
    key(&mut app, &sender, KeyCode::Esc);
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::CancelSourceDiscovery)
    ));
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &sender,
    );
    let BackendRequest::ExploreSources(request) = receiver.try_recv().unwrap() else {
        panic!("Ctrl-P should request the debugger catalog");
    };
    assert_eq!(request.place, Place::Catalog);
    assert_eq!(app.inspection_generation, 0);
}

#[test]
fn opening_a_catalog_source_preserves_inspection_and_execution_target() {
    let mut app = App::new("http://localhost".into());
    app.threads.push(model::ThreadItem {
        id: "thread/selected".into(),
        session_id: "session/selected".into(),
        name: "main".into(),
        state: "stopped".into(),
        function: "work".into(),
        file: Some("/build/selected.rs".into()),
        line: Some(9),
    });
    let generation = app.begin_inspection();
    app.apply_scopes(vec![v2::Scope {
        scope_id: "scope/selected".into(),
        expensive: true,
        ..Default::default()
    }]);
    let (sender, mut receiver) = mpsc::channel(4);
    toggle_selected_variable(&mut app, &sender);
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::ExpandScope { .. })
    ));
    assert!(app.variables[0].loading);
    let request = app.source_explorer.open(true);
    apply_message(
        &mut app,
        UiMessage::SourceDiscovery {
            generation: request.generation,
            result: Ok(source_page()),
        },
        &sender,
    );
    key(&mut app, &sender, KeyCode::Enter);
    let BackendRequest::LoadSource(request) = receiver.try_recv().unwrap() else {
        panic!("opening a catalog row should only read source");
    };
    assert_eq!(
        request.address.id(),
        Some(SourceId::Ddb {
            instance: "test".into(),
            reference: "source/other-session/main".into()
        })
    );
    assert!(!app.source_explorer.visible);
    assert!(!app.source_browser.following());
    assert_eq!(app.current_thread().unwrap().id, "thread/selected");
    assert_eq!(app.inspection_generation, generation);
    assert!(app.variables[0].loading);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn full_queue_releases_discovery_loading_and_refresh_retries() {
    let mut app = App::new("http://localhost".into());
    let (sender, mut receiver) = mpsc::channel(1);
    sender.try_send(BackendRequest::Refresh).unwrap();
    key(&mut app, &sender, KeyCode::Char('o'));
    assert!(!app.source_explorer.loading());
    assert!(app.source_explorer.error.is_some());
    receiver.try_recv().unwrap();
    key(&mut app, &sender, KeyCode::F(5));
    assert!(app.source_explorer.loading());
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::ExploreSources(_))
    ));
}

#[test]
fn explorer_rows_and_close_are_mouse_addressable_in_compact_layouts() {
    for (width, height) in [(40, 10), (60, 18), (89, 23), (140, 40)] {
        let mut app = App::new("http://localhost".into());
        let request = app.source_explorer.open(true);
        app.source_explorer.apply(request.generation, source_page());
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| app.areas = ui::draw(frame, &mut app))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            screen.contains("Source explorer"),
            "{width}x{height}: {screen}"
        );
        assert!(screen.contains("Close"), "{width}x{height}: {screen}");
        assert_eq!(app.areas.source_explorer_rows.len(), 1);
        let close = app
            .areas
            .source_explorer_controls
            .iter()
            .find(|(control, _)| *control == ExplorerControl::Close)
            .unwrap()
            .1;
        assert_eq!(
            app.areas.source_explorer_control_at(close.x, close.y),
            Some(ExplorerControl::Close)
        );
        let (sender, mut receiver) = mpsc::channel(2);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: close.x,
                row: close.y,
                modifiers: KeyModifiers::NONE,
            },
            &sender,
        );
        assert!(!app.source_explorer.visible);
        assert!(matches!(
            receiver.try_recv(),
            Ok(BackendRequest::CancelSourceDiscovery)
        ));

        let request = app.source_explorer.open(true);
        app.source_explorer.apply(request.generation, source_page());
        terminal
            .draw(|frame| app.areas = ui::draw(frame, &mut app))
            .unwrap();
        let (_, row) = app.areas.source_explorer_rows[0];
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: row.x,
                row: row.y,
                modifiers: KeyModifiers::NONE,
            },
            &sender,
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(BackendRequest::LoadSource(_))
        ));
    }
}

#[test]
fn source_panel_has_a_mouse_entry_point_without_a_loaded_document() {
    for (width, height) in [(40, 10), (60, 18), (140, 40)] {
        let mut app = App::new("http://localhost".into());
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| app.areas = ui::draw(frame, &mut app))
            .unwrap();
        let open = app
            .areas
            .source_explorer_open
            .expect("source panel should have Open control");
        let (sender, mut receiver) = mpsc::channel(1);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: open.x,
                row: open.y,
                modifiers: KeyModifiers::NONE,
            },
            &sender,
        );
        assert!(app.source_explorer.visible);
        assert!(matches!(
            receiver.try_recv(),
            Ok(BackendRequest::ExploreSources(_))
        ));
    }
}
