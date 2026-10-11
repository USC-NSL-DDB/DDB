use super::*;
use crate::model::TopologyRowKind;

fn fixture() -> App {
    let mut app = App::new("http://localhost".into());
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend".into(),
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
        ..Default::default()
    });
    app.apply_threads(
        ["a", "b"]
            .into_iter()
            .map(|id| v2::Thread {
                thread_id: format!("thread/{id}"),
                session_id: format!("session/{id}"),
                name: Some(format!("thread-{id}")),
                state: v2::ThreadState::Stopped as i32,
                ..Default::default()
            })
            .collect(),
    );
    app.focus = Focus::Threads;
    app.capabilities.execution_actions = vec![v2::ExecutionAction::Signal as i32];
    app.capabilities.execution_action_capabilities = vec![v2::ExecutionActionCapability {
        action: v2::ExecutionAction::Signal as i32,
        scopes: vec![
            v2::ExecutionScopeKind::Thread as i32,
            v2::ExecutionScopeKind::Session as i32,
        ],
    }];
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

fn key(app: &mut App, tx: &mpsc::Sender<BackendRequest>, code: KeyCode) {
    handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), tx);
}

fn click(app: &mut App, tx: &mpsc::Sender<BackendRequest>, column: u16, row: u16) {
    handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        tx,
    );
}

#[test]
fn topology_folding_preserves_hidden_thread_inspection_scope_and_reconnect_preferences() {
    let mut app = fixture();
    let thread = TopologyRowKind::Thread("thread/b".into());
    app.select_visible_topology(&thread).unwrap();
    let target = app.execution_target();
    let generation = app.inspection_generation;
    let session = TopologyRowKind::Session("session/b".into());
    assert!(app.toggle_topology_fold(&session));
    assert_eq!(app.selected_topology_thread_id(), Some("thread/b"));
    assert_eq!(
        app.topology_rows()[app.selected_topology_row()].kind,
        session
    );
    let group = TopologyRowKind::Group("group/workers".into());
    assert!(app.toggle_topology_fold(&group));
    assert_eq!(app.topology_rows().len(), 1);
    assert_eq!(app.execution_target(), target);
    assert_eq!(app.inspection_generation, generation);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    app.apply_threads(
        ["a", "b"]
            .into_iter()
            .map(|id| v2::Thread {
                thread_id: format!("thread/{id}"),
                session_id: format!("session/{id}"),
                state: v2::ThreadState::Stopped as i32,
                ..Default::default()
            })
            .collect(),
    );
    assert_eq!(app.selected_topology_thread_id(), Some("thread/b"));
    assert_eq!(app.execution_target(), target);
    app.toggle_topology_grouping();
    assert!(app.topology_collapsed(&session));
    assert!(app.topology_collapsed(&group));
    app.toggle_topology_grouping();
    assert_eq!(app.topology_rows().len(), 1);
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "replacement".into(),
        sessions: app.sessions.clone(),
        groups: app.groups.clone(),
        ..Default::default()
    });
    assert!(!app.topology_collapsed(&session));
    assert!(!app.topology_collapsed(&group));
}

#[test]
fn topology_keyboard_tree_navigation_and_shift_scroll_do_not_conflict() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    let group = TopologyRowKind::Group("group/workers".into());
    app.select_visible_topology(&group).unwrap();
    let target = app.execution_target();
    key(&mut app, &tx, KeyCode::Char(' '));
    assert!(app.topology_collapsed(&group));
    assert_eq!(app.execution_target(), target);
    key(&mut app, &tx, KeyCode::Right);
    assert!(!app.topology_collapsed(&group));
    key(&mut app, &tx, KeyCode::Right);
    assert!(
        matches!(app.execution_target().unwrap().selector, Some(v2::target::Selector::Session(value)) if value.session_id == "session/a")
    );
    key(&mut app, &tx, KeyCode::Enter);
    assert!(app.topology_collapsed(&TopologyRowKind::Session("session/a".into())));
    key(&mut app, &tx, KeyCode::Right);
    assert!(
        rx.try_recv().is_err(),
        "folding and session selection are local"
    );
    key(&mut app, &tx, KeyCode::Right);
    assert!(
        matches!(rx.try_recv().unwrap(), BackendRequest::InspectThread { thread_id, .. } if thread_id == "thread/a")
    );
    key(&mut app, &tx, KeyCode::Left);
    assert!(app.selected_topology_thread_id().is_none());
    app.sessions[0].display_name = "long-session-name-".repeat(20);
    render(&mut app, 140, 40);
    let rows = app.topology_rows().len();
    let target = app.execution_target();
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        &tx,
    );
    assert!(app.topology_viewport.column() > 0);
    assert_eq!(app.topology_rows().len(), rows);
    assert_eq!(app.execution_target(), target);
    assert!(rx.try_recv().is_err());
}

#[test]
fn topology_disclosure_clicks_are_local_and_session_label_selects_exact_signal_target() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    render(&mut app, 140, 40);
    let target = app.execution_target();
    let group = TopologyRowKind::Group("group/workers".into());
    let arrow = app
        .areas
        .topology_folds
        .iter()
        .find(|(kind, _)| *kind == group)
        .unwrap()
        .1;
    click(&mut app, &tx, arrow.x, arrow.y);
    assert_eq!(app.topology_rows().len(), 1);
    assert_eq!(app.execution_target(), target);
    assert!(rx.try_recv().is_err());
    render(&mut app, 140, 40);
    let arrow = app.areas.topology_folds[0].1;
    click(&mut app, &tx, arrow.x, arrow.y);
    render(&mut app, 140, 40);
    let session = TopologyRowKind::Session("session/b".into());
    let row = app
        .areas
        .topology_rows
        .iter()
        .find(|(kind, _)| *kind == session)
        .unwrap()
        .1;
    click(&mut app, &tx, row.x + 9, row.y);
    assert!(
        matches!(app.execution_target().unwrap().selector, Some(v2::target::Selector::Session(value)) if value.session_id == "session/b")
    );
    key(&mut app, &tx, KeyCode::Char('s'));
    assert!(
        matches!(rx.try_recv().unwrap(), BackendRequest::ListSignals { target } if matches!(&target.selector, Some(v2::target::Selector::Session(value)) if value.session_id == "session/b"))
    );
    let selected = app.execution_target().unwrap();
    app.open_signal_picker(
        selected,
        vec![v2::DebuggerSignal {
            name: "SIGINT".into(),
            ..Default::default()
        }],
    )
    .unwrap();
    // Fold after opening the catalog. The accepted signal keeps its captured
    // single-session target rather than using the visible ancestor/group.
    app.toggle_topology_fold(&group);
    key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv().unwrap().untracked(), BackendRequest::SendSignal { signal, target }
        if signal == "SIGINT" && matches!(&target.selector, Some(v2::target::Selector::Session(value)) if value.session_id == "session/b"))
    );
}

#[test]
fn topology_mouse_rows_use_identity_across_reordering_and_reject_hidden_children() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    render(&mut app, 140, 40);
    let session = TopologyRowKind::Session("session/b".into());
    let row = app
        .areas
        .topology_rows
        .iter()
        .find(|(kind, _)| *kind == session)
        .unwrap()
        .1;
    app.sessions[0].display_name = "z-worker".into();
    click(&mut app, &tx, row.x + 9, row.y);
    assert!(
        matches!(app.execution_target().unwrap().selector, Some(v2::target::Selector::Session(value)) if value.session_id == "session/b")
    );
    let target = app.execution_target();
    app.toggle_topology_fold(&TopologyRowKind::Group("group/workers".into()));
    click(&mut app, &tx, row.x + 9, row.y);
    assert!(app.status.contains("Topology changed"));
    assert_eq!(app.execution_target(), target);
    assert!(rx.try_recv().is_err());
    for (width, height) in [(8, 4), (35, 10), (140, 40)] {
        render(&mut app, width, height);
    }
}

#[test]
fn topology_fold_state_handles_flat_ungrouped_and_retired_sessions() {
    let mut app = fixture();
    app.groups.clear();
    app.sessions[0].group_id = None;
    let ungrouped = TopologyRowKind::Group(String::new());
    assert!(app.toggle_topology_fold(&ungrouped));
    assert!(app
        .topology_rows()
        .iter()
        .all(|row| row.kind != TopologyRowKind::Session("session/a".into())));
    let unresolved = TopologyRowKind::Group("group/workers".into());
    assert!(app.toggle_topology_fold(&unresolved));
    app.toggle_topology_grouping();
    let session = TopologyRowKind::Session("session/a".into());
    assert!(app.toggle_topology_fold(&session));
    assert!(app
        .topology_index(&TopologyRowKind::Thread("thread/a".into()))
        .is_none());
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend".into(),
        ..Default::default()
    });
    assert!(!app.topology_collapsed(&session));
    assert!(!app.topology_collapsed(&ungrouped));
    assert!(!app.topology_collapsed(&unresolved));
    assert!(!app.toggle_selected_topology_fold());
}
