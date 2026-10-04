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
            .draw(|frame| app.areas = ui::draw(frame, &app))
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
            .draw(|frame| app.areas = ui::draw(frame, &app))
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
            .draw(|frame| app.areas = ui::draw(frame, &app))
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
