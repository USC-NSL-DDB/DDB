use super::*;
use crate::model::{Breakpoint, BreakpointLocation, SourceLocation, ThreadItem};
use crate::source_binding::DebuggerBinding;
use crate::source_browser::tests::{remote_content, remote_source};
use ratatui::{backend::TestBackend, Terminal};

fn file(owners: &[&str], mapped: bool) -> v2::SourceFile {
    v2::SourceFile {
        source_reference: format!("root/{owners:?}/main.rs"),
        path: Some("checkout/main.rs".into()),
        debugger_path: mapped.then(|| "/build/main.rs".into()),
        owners: owners
            .iter()
            .map(|owner| v2::SourceOwner {
                session_id: (*owner).into(),
                group_id: Some("stale-group".into()),
            })
            .collect(),
        ..Default::default()
    }
}

fn app(owners: &[&str], mapped: bool) -> App {
    let mut app = App::new("http://localhost".into());
    app.apply_capabilities(v2::Capabilities {
        supported_operations: vec![
            v2::OperationKind::CreateBreakpoint as i32,
            v2::OperationKind::DeleteBreakpoint as i32,
        ],
        breakpoint_features: vec![
            v2::BreakpointFeature::Source as i32,
            v2::BreakpointFeature::Distributed as i32,
            v2::BreakpointFeature::GroupInheritance as i32,
        ],
        ..Default::default()
    });
    app.sessions = ["a", "b"]
        .into_iter()
        .map(|id| v2::Session {
            session_id: id.into(),
            display_name: id.into(),
            group_id: Some("group".into()),
            ..Default::default()
        })
        .collect();
    app.threads.push(ThreadItem {
        id: "thread/b".into(),
        session_id: "b".into(),
        name: "main".into(),
        state: "stopped".into(),
        function: "work".into(),
        file: Some("/build/main.rs".into()),
        line: Some(2),
    });
    app.execution_location = Some(SourceLocation {
        session_id: "b".into(),
        source: "/build/main.rs".into(),
        line: 2,
    });
    let source = file(owners, mapped);
    let request = app
        .source_browser
        .open_source(remote_source(source.clone()));
    assert!(app.source_browser.apply(
        request.generation,
        remote_content(v2::SourceContent {
            source: Some(source),
            content: "first\nsecond\nthird".into(),
            start_line: 1,
            line_count: 3,
            total_lines: 3,
            has_more: false
        })
    ));
    app.source_browser.set_cursor(2);
    app
}

fn breakpoint(id: &str, target: v2::Target) -> Breakpoint {
    Breakpoint {
        id: id.into(),
        target: Some(target),
        location: BreakpointLocation {
            src: "/build/main.rs".into(),
            line: 2,
        },
        enabled: true,
        times: 0,
        condition: None,
        ignore_count: None,
        temporary: false,
        hardware: false,
        verified: true,
        pending: false,
        message: None,
        sub_breakpoints: Vec::new(),
    }
}

#[test]
fn breakpoint_navigation_selects_exact_child_owner_without_retargeting_execution() {
    let mut app = App::new("http://localhost".into());
    let mut bp = breakpoint("distributed", api::group_target("workers"));
    bp.sub_breakpoints = ["a", "b"]
        .into_iter()
        .map(|owner| v2::SubBreakpoint {
            sub_breakpoint_id: format!("site/{owner}"),
            session_id: owner.into(),
            location: Some(v2::SourceLocation {
                path: Some(format!("/{owner}/main.rs")),
                line: 12,
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect();
    app.breakpoints.push(bp);
    app.execution_target = Some(api::session_target("unrelated"));
    app.focus = Focus::Breakpoints;
    let (tx, mut rx) = mpsc::channel(8);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );
    assert!(
        rx.try_recv().is_err(),
        "aggregate source must not choose an arbitrary site"
    );
    assert!(app.status.contains("session-specific"));
    app.move_selection(2);
    assert_eq!(app.selected_breakpoint_site, Some(1));
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );
    assert!(
        matches!(rx.try_recv().unwrap(), BackendRequest::LoadSource(request)
        if matches!(&request.address, source_types::Address::Resolve { target: Some(target), location, .. }
            if *target == api::session_target("b") && location.path.as_deref() == Some("/b/main.rs")))
    );
    assert_eq!(app.execution_target, Some(api::session_target("unrelated")));
    assert!(!app.source_browser.following());
    app.focus = Focus::Breakpoints;
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| app.areas = ui::draw(frame, &mut app))
        .unwrap();
    let (_, _, area) = *app
        .areas
        .breakpoint_sites
        .iter()
        .find(|(_, site, _)| *site == Some(0))
        .unwrap();
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
        &tx,
    );
    assert!(
        matches!(rx.try_recv().unwrap(), BackendRequest::LoadSource(request)
        if matches!(&request.address, source_types::Address::Resolve { target: Some(target), .. }
            if *target == api::session_target("a")))
    );
}

#[test]
fn bindings_require_exact_debugger_path_and_owner_not_a_display_path_or_suffix() {
    let source = remote_source(file(&["a"], true));
    let binding = DebuggerBinding::from_source(&source, &[]).unwrap();
    assert!(binding.matches_location("a", "/build/main.rs"));
    assert!(!binding.matches_location("b", "/build/main.rs"));
    assert!(!binding.matches_location("a", "main.rs"));
    assert!(!binding.matches_location("a", "checkout/main.rs"));
    assert!(!binding.matches_location("a", "/other/build/main.rs"));
    assert!(DebuggerBinding::from_source(&remote_source(file(&["a"], false)), &[]).is_none());
}

#[test]
fn unscoped_local_paths_project_breakpoints_but_never_infer_an_owner() {
    let mut local = crate::source_types::LocalSource {
        root: crate::source_types::LocalRoot {
            name: "application".into(),
            debugger_path: Some("/build".into()),
            session_tags: vec![],
        },
        path: "main.rs".into(),
        owner_hint: vec![],
        owner_instance: None,
    };
    let source = crate::source_types::Source::Local(local.clone());
    let binding = DebuggerBinding::from_source(&source, &[]).unwrap();
    assert!(binding.single_owner().is_none());
    assert!(binding.marks_breakpoint(
        &breakpoint(
            "future",
            v2::Target {
                selector: Some(v2::target::Selector::Broadcast(Default::default()))
            }
        ),
        2,
        &[]
    ));
    assert!(!binding.matches_location("a", "/other/build/main.rs"));
    local.owner_hint.push(v2::SourceOwner {
        session_id: "a".into(),
        ..Default::default()
    });
    let source = crate::source_types::Source::Local(local.clone());
    let binding = DebuggerBinding::from_source(&source, &[]).unwrap();
    assert!(binding.matches_location("a", "/build/main.rs"));
    assert!(!binding.matches_location("b", "/build/main.rs"));
    local.owner_hint.clear();
    local.root.session_tags = vec!["worker-a".into()];
    let source = crate::source_types::Source::Local(local);
    let binding = DebuggerBinding::from_source(&source, &[]).unwrap();
    assert!(
        !binding.matches_location("b", "/build/main.rs"),
        "an unmatched explicit tag must not become unscoped"
    );
}

#[test]
fn breakpoint_creation_defaults_to_the_document_owner_without_changing_selection() {
    let mut app = app(&["a"], true);
    let (sender, mut receiver) = mpsc::channel(2);
    toggle_breakpoint(&mut app, &sender);
    let BackendRequest::ResolveBreakpointTargets(lookup) = receiver.try_recv().unwrap() else {
        panic!("source breakpoints must discover their targets through DDB")
    };
    assert_eq!(lookup.source, "/build/main.rs");
    finish_targets(&mut app, lookup, &["a"]);
    let picker = app.breakpoint_target_picker.as_ref().unwrap();
    assert!(
        matches!(&picker.draft.location, v2::breakpoint_spec::Location::Source(source)
        if source.source == "/build/main.rs")
    );
    assert_eq!(
        picker
            .choices
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.target.clone())
            .collect::<Vec<_>>(),
        [BreakpointTarget::Session("a".into())]
    );
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &sender,
    );
    assert!(
        matches!(receiver.try_recv().map(BackendRequest::untracked), Ok(BackendRequest::CreateBreakpoint { location: v2::breakpoint_spec::Location::Source(source), target, .. }) if source.source == "/build/main.rs" && target == BreakpointTarget::Session("a".into()))
    );
    assert_eq!(app.current_thread().unwrap().session_id, "b");
}

#[test]
fn unbound_and_multi_owner_documents_require_explicit_breakpoint_scope() {
    for owners in [&[][..], &["a", "b"][..]] {
        let mut app = app(owners, true);
        app.start_breakpoint_target_picker(BreakpointOptions::default())
            .unwrap();
        let lookup = app
            .breakpoint_target_picker
            .as_ref()
            .unwrap()
            .lookup
            .clone()
            .unwrap();
        finish_targets(&mut app, lookup, &["a", "b"]);
        assert!(app
            .breakpoint_target_picker
            .as_ref()
            .unwrap()
            .choices
            .iter()
            .all(|choice| !choice.selected));
        assert!(app
            .commit_breakpoint_target_picker()
            .unwrap_err()
            .contains("select at least one"));
    }
    let mut app = app(&["a"], false);
    assert!(app
        .start_breakpoint_target_picker(BreakpointOptions::default())
        .unwrap_err()
        .contains("no debugger path mapping"));
}

fn finish_targets(app: &mut App, lookup: crate::model::SourceTargetLookup, sessions: &[&str]) {
    let context = lookup
        .instance
        .as_ref()
        .map(|instance| v2::ResponseContext {
            server_instance_id: instance.clone(),
            ..Default::default()
        });
    app.finish_source_targets(
        lookup,
        Ok(v2::ResolveSourceTargetsResponse {
            context,
            targets: sessions
                .iter()
                .map(|id| v2::SourceTarget {
                    target: Some(api::session_target(*id)),
                    display_name: (*id).into(),
                    matching_session_count: 1,
                })
                .collect(),
            complete: true,
            warnings: Vec::new(),
        }),
    );
}

#[test]
fn source_target_errors_and_incomplete_empty_results_are_visible_inside_the_modal() {
    let mut app = app(&["a"], true);
    app.start_breakpoint_target_picker(Default::default())
        .unwrap();
    let lookup = app
        .breakpoint_target_picker
        .as_ref()
        .unwrap()
        .lookup
        .clone()
        .unwrap();
    let render = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| app.areas = ui::draw(frame, app))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    assert!(render(&mut app).contains("Discovering source-relevant"));
    assert!(app.areas.breakpoint_target_apply.is_none());
    app.finish_source_targets(lookup.clone(), Err("request timed out".into()));
    assert!(render(&mut app).contains("request timed out"));
    assert!(!app.breakpoint_target_picker.as_ref().unwrap().loading);
    app.finish_source_targets(
        lookup.clone(),
        Ok(v2::ResolveSourceTargetsResponse {
            complete: false,
            warnings: vec![v2::SourceDiscoveryWarning {
                session_id: "a".into(),
                message: "running target is unknown".into(),
            }],
            ..Default::default()
        }),
    );
    let screen = render(&mut app);
    assert!(screen.contains("Incomplete discovery"));
    assert!(screen.contains("running target is unknown"));
    assert!(!screen.contains("No debugger sessions use"));
    finish_targets(&mut app, lookup, &[]);
    assert!(render(&mut app).contains("No debugger sessions use this exact source path"));
    assert!(app.areas.breakpoint_target_apply.is_none());
}

#[test]
fn full_request_queue_does_not_leave_source_targets_loading() {
    let mut app = app(&["a"], true);
    let (sender, _receiver) = mpsc::channel(1);
    sender.try_send(BackendRequest::Refresh).unwrap();
    toggle_breakpoint(&mut app, &sender);
    let picker = app.breakpoint_target_picker.as_ref().unwrap();
    assert!(!picker.loading);
    assert!(picker
        .message
        .as_ref()
        .unwrap()
        .contains("queue unavailable"));
    assert!(picker.choices.is_empty());
}

#[test]
fn source_targets_are_server_owned_and_old_picker_replies_cannot_replace_them() {
    let mut app = app(&["a"], true);
    app.start_breakpoint_target_picker(Default::default())
        .unwrap();
    let old = app
        .breakpoint_target_picker
        .as_ref()
        .unwrap()
        .lookup
        .clone()
        .unwrap();
    assert!(app
        .breakpoint_target_picker
        .as_ref()
        .unwrap()
        .choices
        .is_empty());
    assert!(app
        .commit_breakpoint_target_picker()
        .unwrap_err()
        .contains("pending"));
    app.cancel_breakpoint_target_picker();
    app.start_breakpoint_target_picker(Default::default())
        .unwrap();
    let current = app
        .breakpoint_target_picker
        .as_ref()
        .unwrap()
        .lookup
        .clone()
        .unwrap();
    finish_targets(&mut app, old, &["a"]);
    assert!(app.breakpoint_target_picker.as_ref().unwrap().loading);
    finish_targets(&mut app, current, &["b"]);
    let picker = app.breakpoint_target_picker.as_ref().unwrap();
    assert_eq!(picker.choices.len(), 1);
    assert_eq!(
        picker.choices[0].target,
        BreakpointTarget::Session("b".into())
    );
    assert!(
        !picker.choices[0].selected,
        "a local owner cannot override DDB's discovery result"
    );
}

#[test]
fn breakpoint_toggle_never_deletes_an_unrelated_owner_or_an_ambiguous_match() {
    let mut app = app(&["a"], true);
    let (sender, mut receiver) = mpsc::channel(2);
    app.breakpoints
        .push(breakpoint("b", api::session_target("b")));
    toggle_breakpoint(&mut app, &sender);
    assert!(matches!(
        receiver.try_recv(),
        Ok(BackendRequest::ResolveBreakpointTargets(_))
    ));
    assert!(app.breakpoint_target_picker.is_some());
    app.cancel_breakpoint_target_picker();
    app.breakpoints
        .push(breakpoint("a", api::session_target("a")));
    toggle_breakpoint(&mut app, &sender);
    assert!(
        matches!(receiver.try_recv().map(BackendRequest::untracked), Ok(BackendRequest::DeleteBreakpoint { id, target }) if id == "a" && target == api::session_target("a"))
    );
    app.breakpoints
        .push(breakpoint("group", api::group_target("group")));
    toggle_breakpoint(&mut app, &sender);
    assert!(receiver.try_recv().is_err());
    assert_eq!(app.focus, Focus::Breakpoints);
    assert_eq!(app.selected_breakpoint, 1);
    assert!(app.status.contains("multiple DDB breakpoints"));
}

#[test]
fn resolved_breakpoints_and_group_membership_preserve_source_ownership() {
    let mut app = app(&["a"], true);
    let source = remote_source(file(&["a"], true));
    let binding = DebuggerBinding::from_source(&source, &[]).unwrap();
    let mut bp = breakpoint("group", api::group_target("group"));
    assert!(binding.marks_breakpoint(&bp, 2, &app.sessions));
    app.sessions[0].group_id = Some("moved".into());
    assert!(!binding.marks_breakpoint(&bp, 2, &app.sessions));
    bp.location.src = "a_function".into();
    bp.sub_breakpoints.push(v2::SubBreakpoint {
        session_id: "b".into(),
        location: Some(v2::SourceLocation {
            path: Some("/build/main.rs".into()),
            line: 3,
            ..Default::default()
        }),
        ..Default::default()
    });
    assert!(!binding.marks_breakpoint(&bp, 3, &app.sessions));
    bp.sub_breakpoints[0].session_id = "a".into();
    assert!(binding.marks_breakpoint(&bp, 3, &app.sessions));
}

#[test]
fn rendered_execution_and_breakpoint_markers_do_not_cross_session_owners() {
    let mut app = app(&["a"], true);
    app.breakpoints
        .push(breakpoint("b", api::session_target("b")));
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal
        .draw(|frame| app.areas = ui::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let source_row = buffer
        .content()
        .chunks(140)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .find(|line| line.contains("2 │ second"))
        .unwrap();
    assert!(source_row.contains("  ▸     2 │ second"), "{source_row}");
    assert!(
        !source_row.contains('▶') && !source_row.contains('◆') && !source_row.contains('●'),
        "{source_row}"
    );
    app.execution_location.as_mut().unwrap().session_id = "a".into();
    app.breakpoints
        .push(breakpoint("a", api::session_target("a")));
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
    assert!(screen.contains("▶●▸     2 │ second"), "{screen}");
}

#[test]
fn backend_replacement_invalidates_source_handles_but_same_instance_refresh_preserves_browsing() {
    let mut app = app(&["a"], true);
    let first = v2::Snapshot {
        server_instance_id: "test".into(),
        sessions: app.sessions.clone(),
        ..Default::default()
    };
    app.apply_snapshot(first.clone());
    app.apply_snapshot(first);
    assert!(app.source_browser.document().is_some());
    let stale_read = app
        .source_browser
        .open_source(remote_source(file(&["a"], true)));
    let stale_search = app.source_explorer.open(true);
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend-b".into(),
        ..Default::default()
    });
    assert!(app.source_browser.document().is_none());
    assert!(app.source_browser.history(false).is_none());
    assert!(!app.source_explorer.visible);
    let new_read = app
        .source_browser
        .open_source(remote_source(file(&["a"], true)));
    let new_search = app.source_explorer.open(true);
    assert!(new_read.generation > stale_read.generation);
    assert!(new_search.generation > stale_search.generation);
    assert!(!app.source_browser.apply(
        stale_read.generation,
        remote_content(v2::SourceContent {
            source: Some(file(&["a"], true)),
            ..Default::default()
        })
    ));
    assert!(!app.source_explorer.apply(
        stale_search.generation,
        crate::source_explorer::Page {
            entries: Vec::new(),
            next_token: None,
            warnings: Vec::new()
        }
    ));
    assert_eq!(
        app.source_browser.pending_generation(),
        Some(new_read.generation)
    );
}
