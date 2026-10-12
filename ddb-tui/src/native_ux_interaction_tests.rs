use super::*;
use crate::model::{Breakpoint, BreakpointLocation};

fn snapshot(stopped: &[&str], revision: u64) -> v2::Snapshot {
    v2::Snapshot {
        server_instance_id: "backend/native-ux".into(),
        groups: vec![v2::Group {
            group_id: "workers".into(),
            display_name: "Workers".into(),
            ..Default::default()
        }],
        sessions: ["a", "b"]
            .into_iter()
            .map(|id| v2::Session {
                session_id: id.into(),
                display_name: format!("worker-{id}"),
                group_id: Some("workers".into()),
                ..Default::default()
            })
            .collect(),
        threads: ["a", "b"]
            .into_iter()
            .map(|id| v2::Thread {
                thread_id: format!("thread/{id}"),
                session_id: id.into(),
                name: Some("main".into()),
                state: if stopped.contains(&id) {
                    v2::ThreadState::Stopped
                } else {
                    v2::ThreadState::Running
                } as i32,
                location: Some(v2::SourceLocation {
                    path: Some("/build/main.rs".into()),
                    line: 2,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
        execution_states: ["a", "b"]
            .into_iter()
            .map(|id| v2::ExecutionState {
                execution_state_id: format!("execution/{id}"),
                target: Some(api::thread_target(format!("thread/{id}"))),
                running: !stopped.contains(&id),
                revision,
                stop_reason: stopped.contains(&id).then(|| v2::StopReason {
                    kind: v2::StopReasonKind::Breakpoint as i32,
                    thread_id: Some(format!("thread/{id}")),
                    breakpoint_id: Some("breakpoint/shared".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
        breakpoints: vec![v2::Breakpoint {
            breakpoint_id: "breakpoint/shared".into(),
            spec: Some(v2::BreakpointSpec {
                location: Some(v2::breakpoint_spec::Location::Source(
                    v2::SourceBreakpointLocation {
                        source: "/build/main.rs".into(),
                        line: 2,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            target: Some(api::session_target("a")),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn app(stopped: &[&str]) -> App {
    let mut app = App::new("http://localhost".into());
    let snapshot = snapshot(stopped, 1);
    app.apply_snapshot(snapshot.clone());
    app.apply_threads(snapshot.threads);
    app.capabilities.supported_operations = vec![
        v2::OperationKind::CreateBreakpoint as i32,
        v2::OperationKind::DeleteBreakpoint as i32,
    ];
    app.capabilities.breakpoint_features = vec![v2::BreakpointFeature::Source as i32];
    app.capabilities.execution_actions = vec![
        v2::ExecutionAction::Continue as i32,
        v2::ExecutionAction::Interrupt as i32,
        v2::ExecutionAction::Next as i32,
    ];
    app.capabilities.execution_action_capabilities = app
        .capabilities
        .execution_actions
        .iter()
        .map(|action| v2::ExecutionActionCapability {
            action: *action,
            scopes: vec![
                v2::ExecutionScopeKind::Thread as i32,
                v2::ExecutionScopeKind::Fanout as i32,
            ],
        })
        .collect();
    app
}

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(160, 42)).unwrap();
    terminal
        .draw(|frame| app.areas = ui::draw(frame, app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(160)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn local_config_source_marks_and_removes_the_created_breakpoint_without_owner_tags() {
    let mut app = app(&["a", "b"]);
    let source = source_types::Source::Local(source_types::LocalSource {
        root: source_types::LocalRoot {
            name: "application".into(),
            debugger_path: Some("/build".into()),
            session_tags: vec![],
        },
        path: "main.rs".into(),
        owner_hint: vec![],
        owner_instance: None,
    });
    let request = app.source_browser.open_source(source.clone());
    assert!(app.source_browser.apply(
        request.generation,
        source_types::Content {
            source,
            start_line: 1,
            content: "first\nsecond\nthird".into(),
            line_count: 3,
            total_lines: 3,
        }
    ));
    app.source_browser.set_cursor(2);
    app.breakpoints = vec![Breakpoint {
        id: "breakpoint/shared".into(),
        target: Some(api::session_target("a")),
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
        sub_breakpoints: vec![],
    }];
    let screen = render(&mut app);
    let source_row = screen
        .lines()
        .find(|row| row.contains("2 │ second"))
        .unwrap();
    assert!(
        source_row.contains('●'),
        "the mapped local source must show the breakpoint: {source_row}"
    );
    let (tx, mut rx) = mpsc::channel(8);
    toggle_breakpoint(&mut app, &tx);
    assert!(
        matches!(rx.try_recv().map(BackendRequest::untracked), Ok(BackendRequest::DeleteBreakpoint { id, .. }) if id == "breakpoint/shared")
    );
}

#[test]
fn controls_continue_all_by_default_but_step_the_selected_thread() {
    let mut app = app(&["a", "b"]);
    app.selected_thread = 1;
    let (tx, mut rx) = mpsc::channel(8);
    dispatch_control(&mut app, &tx, Control::Continue);
    assert!(
        matches!(rx.try_recv().map(BackendRequest::untracked), Ok(BackendRequest::Control(Control::Continue, target)) if matches!(target.selector, Some(v2::target::Selector::Broadcast(_))))
    );
    dispatch_control(&mut app, &tx, Control::Interrupt);
    assert!(
        matches!(rx.try_recv().map(BackendRequest::untracked), Ok(BackendRequest::Control(Control::Interrupt, target)) if matches!(target.selector, Some(v2::target::Selector::Broadcast(_))))
    );
    dispatch_control(&mut app, &tx, Control::Next);
    assert!(
        matches!(rx.try_recv().map(BackendRequest::untracked), Ok(BackendRequest::Control(Control::Next, target)) if target == api::thread_target("thread/b"))
    );
}

#[test]
fn first_breakpoint_stop_selects_its_topology_and_later_concurrent_stops_do_not_steal_it() {
    let mut app = app(&[]);
    let (tx, mut rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(snapshot(&["b"], 2)), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    assert_eq!(app.selected_topology_thread_id(), Some("thread/b"));
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|request| matches!(request.untracked(), BackendRequest::InspectThread { thread_id, .. } if thread_id == "thread/b")));
    apply_message(&mut app, UiMessage::Snapshot(snapshot(&["a", "b"], 3)), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    assert_eq!(app.breakpoint_hits().len(), 2);
    assert!(render(&mut app).contains("▶ 2 paused"));
}

#[test]
fn pending_actions_have_a_visible_activity_indicator() {
    let mut app = app(&["a"]);
    let (tx, _rx) = mpsc::channel(8);
    dispatch_control(&mut app, &tx, Control::Next);
    let screen = render(&mut app);
    assert!(
        screen.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(ch)),
        "an active command needs an animated activity indicator"
    );
    app.operations.tick();
    let next = render(&mut app);
    assert_ne!(
        screen, next,
        "the indicator must advance without another API event"
    );
}

#[test]
fn automatic_step_uses_the_distributed_frame_owner_and_rejects_boundaries() {
    let mut app = app(&["a", "b"]);
    app.apply_frames(vec![v2::Frame {
        frame_id: "frame/b".into(),
        thread_id: "thread/b".into(),
        ..Default::default()
    }]);
    assert_eq!(
        app.control_target("next"),
        Some(api::thread_target("thread/b"))
    );
    app.frames[0].boundary = true;
    assert!(app.control_target("next").is_none());
    assert!(matches!(
        app.control_target("continue").unwrap().selector,
        Some(v2::target::Selector::Broadcast(_))
    ));
}

#[test]
fn control_override_is_explicit_and_cycles_back_to_automatic_defaults() {
    let mut app = app(&["a", "b"]);
    assert!(!app.execution_scope_override);
    app.cycle_execution_scope();
    assert!(app.execution_scope_override);
    assert_eq!(
        app.control_target("continue"),
        Some(api::thread_target("thread/a"))
    );
    for _ in 0..4 {
        app.cycle_execution_scope();
    }
    assert!(!app.execution_scope_override);
    assert_eq!(app.control_scope_label(), "Auto");
    assert!(matches!(
        app.control_target("continue").unwrap().selector,
        Some(v2::target::Selector::Broadcast(_))
    ));
}

#[test]
fn pause_unfolds_the_owner_and_requests_its_source_even_after_manual_browsing() {
    let mut app = app(&[]);
    app.source_browser.set_following(false);
    assert!(app.toggle_topology_fold(&crate::model::TopologyRowKind::Group("workers".into())));
    let (tx, mut rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(snapshot(&["b"], 2)), &tx);
    assert!(app.source_browser.following());
    assert!(!app.topology_collapsed(&crate::model::TopologyRowKind::Group("workers".into())));
    let generation = app.inspection_generation;
    while rx.try_recv().is_ok() {}
    apply_message(
        &mut app,
        UiMessage::DistributedFrames {
            generation,
            thread_id: "thread/b".into(),
            result: v2::DistributedBacktraceResult {
                frames: vec![v2::DistributedFrame {
                    session_id: "b".into(),
                    thread_id: "thread/b".into(),
                    frame: Some(v2::Frame {
                        frame_id: "frame/b".into(),
                        thread_id: "thread/b".into(),
                        location: Some(v2::SourceLocation {
                            path: Some("/build/main.rs".into()),
                            line: 2,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
        },
        &tx,
    );
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|request| matches!(request, BackendRequest::LoadSource(request)
        if matches!(&request.address, source_types::Address::Resolve { target: Some(target), location, .. }
            if *target == api::thread_target("thread/b") && location.path.as_deref() == Some("/build/main.rs") && location.line == 2))));
}

#[test]
fn inspection_progress_clears_on_empty_results_errors_and_failed_queue_admission() {
    let mut app = app(&["a"]);
    let (tx, mut rx) = mpsc::channel(8);
    inspect_thread(&mut app, &tx, "thread/a".into());
    assert!(app.has_activity());
    assert!(render(&mut app).contains("Loading frame and variables"));
    rx.try_recv().unwrap();
    let generation = app.inspection_generation;
    apply_message(
        &mut app,
        UiMessage::DistributedFrames {
            generation,
            thread_id: "thread/a".into(),
            result: Default::default(),
        },
        &tx,
    );
    assert!(
        !app.has_activity(),
        "a valid empty stack is a completed read"
    );
    let (full, _rx) = mpsc::channel(1);
    full.try_send(BackendRequest::Refresh).unwrap();
    inspect_thread(&mut app, &full, "thread/a".into());
    assert!(
        !app.has_activity(),
        "a read that was not queued cannot remain busy"
    );
    inspect_thread(&mut app, &tx, "thread/a".into());
    let generation = app.inspection_generation;
    apply_message(
        &mut app,
        UiMessage::InspectionError {
            generation,
            thread_id: "thread/a".into(),
            error: "unavailable".into(),
        },
        &tx,
    );
    assert!(!app.has_activity());

    inspect_thread(&mut app, &tx, "thread/a".into());
    app.backend_unavailable("connection lost");
    assert!(!app.has_activity(), "disconnect must stop read animation");

    inspect_thread(&mut app, &tx, "thread/a".into());
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend/native-ux".into(),
        ..Default::default()
    });
    assert!(
        !app.has_activity(),
        "retiring the last session ends its read"
    );
}

#[test]
fn a_rejected_execution_request_preserves_the_first_pause_owner() {
    let mut app = app(&["a", "b"]);
    app.select_pause_thread("thread/a");
    let (tx, _rx) = mpsc::channel(1);
    tx.try_send(BackendRequest::Refresh).unwrap();
    dispatch_control(&mut app, &tx, Control::Continue);
    assert_eq!(app.pause_focus.as_deref(), Some("thread/a"));
    assert!(!app.select_pause_thread("thread/b"));
    assert_eq!(app.current_thread().unwrap().id, "thread/a");
}

#[test]
fn a_running_snapshot_releases_pause_focus_before_a_later_process_wide_stop() {
    let mut app = app(&["a"]);
    app.select_pause_thread("thread/a");
    let (tx, _rx) = mpsc::channel(64);

    // A snapshot can overtake the owner's running event after Continue.
    apply_message(&mut app, UiMessage::Snapshot(snapshot(&[], 2)), &tx);
    assert!(app.pause_focus.is_none());

    // The next RPC breakpoint stops the whole server process. Its main
    // thread is stopped again, but only the RPC thread owns the new cause.
    let mut stopped = snapshot(&["a"], 3);
    let mut rpc = stopped.threads[0].clone();
    rpc.thread_id = "thread/rpc".into();
    rpc.name = Some("rpc".into());
    stopped.threads.push(rpc);
    let mut execution = stopped.execution_states[0].clone();
    execution.execution_state_id = "execution/rpc".into();
    execution.target = Some(api::thread_target("thread/rpc"));
    execution.stop_reason.as_mut().unwrap().thread_id = Some("thread/rpc".into());
    stopped.execution_states[0].stop_reason = execution.stop_reason.clone();
    stopped.execution_states.push(execution);
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);

    assert_eq!(app.current_thread().unwrap().id, "thread/rpc");
    assert_eq!(app.pause_focus.as_deref(), Some("thread/rpc"));
}

#[test]
fn ordinary_action_feedback_does_not_expose_internal_ids() {
    let mut app = app(&["a"]);
    let (tx, mut rx) = mpsc::channel(8);
    dispatch_control(&mut app, &tx, Control::Next);
    let BackendRequest::Tracked { ticket, .. } = rx.try_recv().unwrap() else {
        panic!()
    };
    apply_message(
        &mut app,
        UiMessage::OperationFeedback {
            ticket,
            update: operation_feedback::Update::Admitted(v2::Operation {
                operation_id: "operation/private-internal-id".into(),
                state: v2::OperationState::Accepted as i32,
                ..Default::default()
            }),
        },
        &tx,
    );
    let screen = render(&mut app);
    assert!(!screen.contains("operation/private-internal-id"));
    assert!(!screen.contains("#1 pending"));
}
