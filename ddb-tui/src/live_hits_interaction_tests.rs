use super::*;

fn snapshot(instance: &str) -> v2::Snapshot {
    v2::Snapshot {
        server_instance_id: instance.into(),
        groups: vec![v2::Group {
            group_id: "group/workers".into(),
            display_name: "Workers".into(),
            ..Default::default()
        }],
        sessions: ["a", "b"]
            .into_iter()
            .map(|id| v2::Session {
                session_id: format!("session/{id}"),
                display_name: format!("worker-{id}"),
                group_id: Some("group/workers".into()),
                ..Default::default()
            })
            .collect(),
        breakpoints: ["shared", "unhit"]
            .into_iter()
            .map(|id| v2::Breakpoint {
                breakpoint_id: format!("breakpoint/{id}"),
                spec: Some(v2::BreakpointSpec {
                    location: Some(v2::breakpoint_spec::Location::Function(
                        v2::FunctionBreakpointLocation {
                            function_name: format!("{id}_function"),
                        },
                    )),
                    ..Default::default()
                }),
                hit_count: 99,
                ..Default::default()
            })
            .collect(),
        execution_states: ["a", "b"]
            .into_iter()
            .map(|id| v2::ExecutionState {
                execution_state_id: format!("execution/{id}"),
                target: Some(thread_target(format!("thread/{id}"))),
                revision: 5,
                stop_reason: Some(v2::StopReason {
                    kind: v2::StopReasonKind::Breakpoint as i32,
                    thread_id: Some(format!("thread/{id}")),
                    breakpoint_id: Some("breakpoint/shared".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn fixture() -> App {
    let mut app = App::new("http://localhost".into());
    app.apply_snapshot(snapshot("backend"));
    app.apply_threads(
        ["a", "b"]
            .into_iter()
            .map(|id| v2::Thread {
                thread_id: format!("thread/{id}"),
                session_id: format!("session/{id}"),
                state: v2::ThreadState::Stopped as i32,
                location: Some(v2::SourceLocation {
                    path: Some("/src/main.cc".into()),
                    line: 42,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
    );
    app.focus = Focus::Breakpoints;
    app.selected_breakpoint = 1;
    app
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| app.areas = ui::draw(frame, app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn live_hits_keyboard_renders_concurrent_stops_and_inspects_the_selected_owner() {
    let mut app = fixture();
    let target = app.execution_target();
    let (tx, mut rx) = mpsc::channel(8);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('H'), KeyModifiers::NONE),
        &tx,
    );
    app.expanded_pane = true;
    let screen = render(&mut app, 100, 25);
    for expected in [
        "DDB Breakpoint Hits",
        "Workers",
        "worker-a",
        "worker-b",
        "thread/a",
        "thread/b",
    ] {
        assert!(screen.contains(expected), "missing {expected}");
    }
    assert!(!screen.contains("unhit_function"));
    assert_eq!(app.areas.current_hit_rows.len(), 2);
    assert!(app.areas.breakpoint_rows.is_empty());
    assert!(rx.try_recv().is_err());
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &tx,
    );
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );
    assert!(
        matches!(rx.try_recv().unwrap(), BackendRequest::InspectThread { thread_id, .. } if thread_id == "thread/b")
    );
    assert_eq!(app.execution_target(), target);
    assert_eq!(app.selected_breakpoint, 1);
    assert_eq!(app.focus, Focus::Source);
}

#[test]
fn live_hits_mouse_checks_rendered_stop_identity_and_ignores_headers() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    render(&mut app, 140, 40);
    let toggle = app.areas.breakpoint_hit_view_toggle.unwrap();
    let click = |column, row| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    handle_mouse(&mut app, click(toggle.x, toggle.y), &tx);
    app.expanded_pane = true;
    render(&mut app, 100, 25);
    let row = app
        .areas
        .current_hit_rows
        .iter()
        .find(|(hit, _)| hit.thread_id == "thread/b")
        .unwrap()
        .1;
    let panel = app
        .areas
        .panels
        .iter()
        .find(|panel| panel.focus == Focus::Breakpoints)
        .unwrap()
        .area;
    handle_mouse(&mut app, click(panel.x + 2, panel.y + 1), &tx);
    assert!(rx.try_recv().is_err(), "group headers must not navigate");
    app.mark_running(Some("thread/b"));
    handle_mouse(&mut app, click(row.x, row.y), &tx);
    assert!(app.status.contains("same stop"));
    assert!(
        rx.try_recv().is_err(),
        "stale rendered row must not redirect navigation"
    );
    render(&mut app, 100, 25);
    assert_eq!(app.areas.current_hit_rows.len(), 1);
    let row = app.areas.current_hit_rows[0].1;
    handle_mouse(&mut app, click(row.x, row.y), &tx);
    assert!(
        matches!(rx.try_recv().unwrap(), BackendRequest::InspectThread { thread_id, .. } if thread_id == "thread/a")
    );
}

#[test]
fn live_hits_protects_hidden_breakpoints_and_handles_replacement_and_tiny_layouts() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    app.toggle_hit_view();
    for key in [
        KeyCode::Delete,
        KeyCode::Char(' '),
        KeyCode::Char('x'),
        KeyCode::Char('L'),
        KeyCode::Char('v'),
    ] {
        handle_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE), &tx);
        assert!(app.status.contains("navigation-only"));
    }
    assert!(rx.try_recv().is_err());
    app.apply_snapshot(snapshot("replacement"));
    assert!(app.commit_visible_hit().is_err());
    assert_eq!(app.selected_breakpoint, 1);
    for (width, height) in [(8, 4), (35, 10), (140, 40)] {
        render(&mut app, width, height);
    }
    app.backend_unavailable("offline");
    app.expanded_pane = true;
    assert!(render(&mut app, 100, 20).contains("Disconnected: no verified current hits"));
    app.api_connected = true;
    app.mark_running(None);
    assert!(render(&mut app, 100, 20).contains("No current breakpoint hits"));
    app.toggle_hit_view();
    assert_eq!(app.current_breakpoint().unwrap().id, "breakpoint/unhit");
}
