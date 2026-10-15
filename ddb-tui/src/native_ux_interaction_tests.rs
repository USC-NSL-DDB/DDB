use super::*;
use crate::model::{Breakpoint, BreakpointLocation};

#[test]
fn backend_policy_projects_logpoints_pause_provenance_and_failures_without_peer_commands() {
    let mut app = app(&[]);
    let mut stopped = snapshot(&["a", "b"], 2);
    stopped.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Signal as i32,
        signal_name: Some("SIGINT".into()),
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    stopped.breakpoints[0].spec.as_mut().unwrap().log_message = Some("value={counter}".into());
    stopped.stop_policy = Some(v2::StopPolicy {
        stop_policy_id: "policy/shared".into(),
        mode: v2::StopMode::AllStop as i32,
        initiating_stop: Some(stopped.execution_states[1].clone()),
        generation: 1,
        revision: 2,
        target_outcomes: vec![v2::TargetOutcome {
            target: Some(api::session_target("a")),
            succeeded: true,
            error: None,
        }],
        ..Default::default()
    });
    let (tx, mut rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    assert!(app.is_automatic_pause("thread/a"));
    assert!(app.logpoint_ids.contains("breakpoint/shared"));
    assert!(!std::iter::from_fn(|| rx.try_recv().ok())
        .map(BackendRequest::untracked)
        .any(|request| matches!(request, BackendRequest::Control(_, _))));
    let mut failed = snapshot(&["b"], 3);
    failed.stop_policy = Some(v2::StopPolicy {
        stop_policy_id: "policy/shared".into(),
        mode: v2::StopMode::AllStop as i32,
        initiating_stop: Some(failed.execution_states[1].clone()),
        generation: 2,
        revision: 3,
        target_outcomes: vec![v2::TargetOutcome {
            target: Some(api::session_target("a")),
            succeeded: false,
            error: Some(v2::DdbError {
                message: "peer unavailable".into(),
                ..Default::default()
            }),
        }],
        ..Default::default()
    });
    apply_message(&mut app, UiMessage::Snapshot(failed), &tx);
    assert!(
        app.timeline
            .visible(160, 30, &mut app.timeline_scroll)
            .iter()
            .any(|line| line.contains("peer unavailable")),
        "the shared failure must remain in the timeline while inspection loads"
    );
    assert!(!app.is_automatic_pause("thread/a"));
    assert!(app.logpoint_ids.is_empty());
}

#[test]
fn queued_resume_captures_the_backend_instance_and_observed_stop() {
    let mut app = app(&["b"]);
    let (tx, mut rx) = mpsc::channel(4);
    queue_request(
        &mut app,
        &tx,
        BackendRequest::Control(Control::Next, api::thread_target("thread/b")),
    );
    let BackendRequest::Tracked { guard, .. } = rx.try_recv().unwrap() else {
        panic!("missing intent envelope")
    };
    assert_eq!(
        guard.server_instance_id.as_deref(),
        Some("backend/native-ux")
    );
    let stop = guard.execution_guard.unwrap();
    assert_eq!(stop.execution_state_id, "execution/b");
    assert_eq!(stop.revision, 1);
    assert_eq!(stop.server_instance_id, "backend/native-ux");
}

#[test]
fn historical_peer_pause_success_cannot_hide_a_later_independent_pause() {
    let mut app = app(&[]);
    let mut stopped = snapshot(&["a", "b"], 2);
    stopped.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Pause as i32,
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    stopped.stop_policy = Some(v2::StopPolicy {
        generation: 1,
        revision: 2,
        initiating_stop: Some(stopped.execution_states[1].clone()),
        target_outcomes: vec![v2::TargetOutcome {
            target: Some(api::session_target("a")),
            succeeded: true,
            error: None,
        }],
        ..Default::default()
    });
    let (tx, mut rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(app.is_automatic_pause("thread/a"));
    while rx.try_recv().is_ok() {}
    let mut running = snapshot(&["b"], 3);
    running.stop_policy = stopped.stop_policy.clone();
    apply_message(&mut app, UiMessage::Snapshot(running), &tx);
    assert!(!app.is_automatic_pause("thread/a"));
    stopped.execution_states[0].revision = 4;
    // Another peer's completion can change the policy revision without creating
    // another attempt for a. Historical success must not be rearmed either way.
    for policy_revision in [2, 3] {
        stopped.stop_policy.as_mut().unwrap().revision = policy_revision;
        apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
        assert!(!app.is_automatic_pause("thread/a"));
        assert!(app
            .topology_stop_label(
                app.threads
                    .iter()
                    .find(|thread| thread.id == "thread/a")
                    .unwrap()
            )
            .is_some());
    }
    let mut retry = snapshot(&["b"], 5);
    retry.stop_policy = stopped.stop_policy;
    let policy = retry.stop_policy.as_mut().unwrap();
    policy.revision = 4;
    policy.target_outcomes.clear();
    policy.pending_targets.push(api::session_target("a"));
    apply_message(&mut app, UiMessage::Snapshot(retry.clone()), &tx);
    retry.execution_states[0].running = false;
    retry.execution_states[0].revision = 6;
    retry.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Pause as i32,
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    retry.threads[0].state = v2::ThreadState::Stopped as i32;
    apply_message(&mut app, UiMessage::Snapshot(retry), &tx);
    assert!(
        app.is_automatic_pause("thread/a"),
        "a new pending attempt must still be projected"
    );
}

#[test]
fn failed_or_retired_pending_peer_pause_cannot_hide_a_manual_pause() {
    for failed in [false, true] {
        let mut app = app(&[]);
        let mut pending = snapshot(&["b"], 2);
        pending.stop_policy = Some(v2::StopPolicy {
            generation: 1,
            revision: 2,
            initiating_stop: Some(pending.execution_states[1].clone()),
            pending_targets: vec![api::session_target("a")],
            ..Default::default()
        });
        let (tx, _rx) = mpsc::channel(32);
        apply_message(&mut app, UiMessage::Snapshot(pending.clone()), &tx);
        let policy = pending.stop_policy.as_mut().unwrap();
        policy.revision += 1;
        policy.pending_targets.clear();
        if failed {
            policy.target_outcomes.push(v2::TargetOutcome {
                target: Some(api::session_target("a")),
                succeeded: false,
                error: Some(v2::DdbError {
                    message: "peer unavailable".into(),
                    ..Default::default()
                }),
            });
        } else {
            policy.generation += 1;
            policy.initiating_stop = None;
        }
        apply_message(&mut app, UiMessage::Snapshot(pending.clone()), &tx);
        let mut manual = snapshot(&["a", "b"], 3);
        manual.stop_policy = pending.stop_policy;
        manual.execution_states[0].stop_reason = Some(v2::StopReason {
            kind: v2::StopReasonKind::Pause as i32,
            thread_id: Some("thread/a".into()),
            ..Default::default()
        });
        apply_message(&mut app, UiMessage::Snapshot(manual), &tx);
        assert!(
            !app.is_automatic_pause("thread/a"),
            "a failed or retired attempt must not label a later pause automatic"
        );
        assert!(app
            .topology_stop_label(
                app.threads
                    .iter()
                    .find(|thread| thread.id == "thread/a")
                    .unwrap()
            )
            .is_some());
    }
}

#[test]
fn successful_pending_peer_pause_keeps_exact_stop_provenance_after_retirement() {
    let mut app = app(&[]);
    let mut pending = snapshot(&["b"], 2);
    pending.stop_policy = Some(v2::StopPolicy {
        generation: 1,
        revision: 2,
        initiating_stop: Some(pending.execution_states[1].clone()),
        pending_targets: vec![api::session_target("a")],
        ..Default::default()
    });
    let (tx, _rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(pending.clone()), &tx);
    let policy = pending.stop_policy.as_mut().unwrap();
    policy.revision += 1;
    policy.pending_targets.clear();
    policy.target_outcomes.push(v2::TargetOutcome {
        target: Some(api::session_target("a")),
        succeeded: true,
        error: None,
    });
    apply_message(&mut app, UiMessage::Snapshot(pending.clone()), &tx);
    let mut stopped = snapshot(&["a", "b"], 3);
    stopped.stop_policy = pending.stop_policy;
    stopped.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Pause as i32,
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(app.is_automatic_pause("thread/a"));
    let policy = stopped.stop_policy.as_mut().unwrap();
    policy.revision += 1;
    policy.generation += 1;
    policy.initiating_stop = None;
    policy.target_outcomes.clear();
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert!(
        app.is_automatic_pause("thread/a"),
        "an already observed automatic stop retains its exact revision"
    );
}

fn authoritative_pause_snapshot(revision: u64) -> v2::Snapshot {
    let mut value = snapshot(&["a", "b"], revision);
    value.capabilities = Some(v2::Capabilities {
        ddb_features: vec!["automatic_stop_provenance".into()],
        ..Default::default()
    });
    value.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Pause as i32,
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    value.stop_policy = Some(v2::StopPolicy {
        stop_policy_id: "policy/shared".into(),
        mode: v2::StopMode::AllStop as i32,
        generation: 1,
        revision,
        initiating_stop: Some(value.execution_states[1].clone()),
        automatic_stops: vec![value.execution_states[0].clone()],
        ..Default::default()
    });
    value
}

#[test]
fn authoritative_pause_provenance_survives_failed_ack_hydration_and_reconfiguration() {
    let mut app = app(&[]);
    let mut pending = authoritative_pause_snapshot(2);
    pending.threads[0].state = v2::ThreadState::Running as i32;
    pending.execution_states[0].running = true;
    pending.execution_states[0].stop_reason = None;
    let policy = pending.stop_policy.as_mut().unwrap();
    policy.automatic_stops.clear();
    policy.pending_targets.push(api::session_target("a"));
    let (tx, _rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(pending.clone()), &tx);
    let failure = v2::TargetOutcome {
        target: Some(api::session_target("a")),
        succeeded: false,
        error: Some(v2::DdbError {
            message: "pause acknowledgment lost".into(),
            ..Default::default()
        }),
    };
    let policy = pending.stop_policy.as_mut().unwrap();
    policy.revision += 1;
    policy.pending_targets.clear();
    policy.target_outcomes.push(failure.clone());
    apply_message(&mut app, UiMessage::Snapshot(pending), &tx);
    let mut late = authoritative_pause_snapshot(3);
    late.stop_policy
        .as_mut()
        .unwrap()
        .target_outcomes
        .push(failure);
    apply_message(&mut app, UiMessage::Snapshot(late.clone()), &tx);
    assert!(app.is_automatic_pause("thread/a"));
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    let mut newcomer = self::app(&[]);
    apply_message(&mut newcomer, UiMessage::Snapshot(late.clone()), &tx);
    assert!(newcomer.is_automatic_pause("thread/a"));
    assert_eq!(newcomer.current_thread().unwrap().id, "thread/b");
    let policy = late.stop_policy.as_mut().unwrap();
    policy.mode = v2::StopMode::Independent as i32;
    policy.generation += 1;
    policy.revision += 1;
    policy.initiating_stop = None;
    policy.target_outcomes.clear();
    apply_message(&mut app, UiMessage::Snapshot(late), &tx);
    assert!(app.is_automatic_pause("thread/a"));
}

#[test]
fn authoritative_pause_classification_requires_the_current_stopped_execution_version() {
    let mut app = app(&[]);
    let mut stopped = authoritative_pause_snapshot(2);
    let (tx, _rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(app.is_automatic_pause("thread/a"));
    // Inspection can advance the public revision while the native stop stays.
    stopped.execution_states[0].revision = 3;
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(!app.is_automatic_pause("thread/a"));
    stopped.stop_policy.as_mut().unwrap().automatic_stops =
        vec![stopped.execution_states[0].clone()];
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(app.is_automatic_pause("thread/a"));
    stopped.execution_states[0].execution_state_id = "execution/replaced".into();
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(!app.is_automatic_pause("thread/a"));
    stopped.execution_states[0].execution_state_id = "execution/a".into();
    stopped.execution_states[0].running = true;
    stopped.threads[0].state = v2::ThreadState::Running as i32;
    apply_message(&mut app, UiMessage::Snapshot(stopped.clone()), &tx);
    assert!(!app.is_automatic_pause("thread/a"));
    stopped.execution_states[0].running = false;
    stopped.execution_states[0].revision = 4;
    stopped.threads[0].state = v2::ThreadState::Stopped as i32;
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert!(
        !app.is_automatic_pause("thread/a"),
        "a later independent pause cannot inherit an older classification"
    );
}

#[test]
fn authoritative_pause_projection_does_not_infer_stops_from_command_acknowledgments() {
    for pending in [false, true] {
        let mut app = app(&[]);
        let mut stopped = authoritative_pause_snapshot(2);
        let policy = stopped.stop_policy.as_mut().unwrap();
        policy.automatic_stops.clear();
        if pending {
            policy.pending_targets.push(api::session_target("a"));
        } else {
            policy.target_outcomes.push(v2::TargetOutcome {
                target: Some(api::session_target("a")),
                succeeded: true,
                error: None,
            });
        }
        let (tx, _rx) = mpsc::channel(32);
        apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
        assert!(!app.is_automatic_pause("thread/a"));
    }
}

#[test]
fn authoritative_classification_corrects_focus_chosen_before_the_policy_event() {
    let mut classified = authoritative_pause_snapshot(2);
    classified.execution_states[1].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Pause as i32,
        thread_id: Some("thread/b".into()),
        ..Default::default()
    });
    classified.stop_policy.as_mut().unwrap().initiating_stop =
        Some(classified.execution_states[1].clone());
    for navigated in [false, true] {
        let mut app = app(&[]);
        let mut unclassified = classified.clone();
        unclassified
            .stop_policy
            .as_mut()
            .unwrap()
            .automatic_stops
            .clear();
        let (tx, _rx) = mpsc::channel(32);
        apply_message(&mut app, UiMessage::Snapshot(unclassified), &tx);
        assert_eq!(app.current_thread().unwrap().id, "thread/a");
        if navigated {
            app.selected_thread = 1;
        }
        apply_message(&mut app, UiMessage::Snapshot(classified.clone()), &tx);
        assert!(app.is_automatic_pause("thread/a"));
        assert_eq!(
            app.current_thread().unwrap().id,
            "thread/b",
            "a policy-only update must restore the initiating stop's focus"
        );
        assert_eq!(
            app.pause_focus.as_deref(),
            (!navigated).then_some("thread/b"),
            "classification must not establish new stop focus after manual navigation"
        );
    }
}

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
    assert!(render(&mut app).contains("[2 hits]"));
}

#[test]
fn stop_event_before_snapshot_selects_its_owner_before_an_automatic_peer() {
    let mut app = app(&[]);
    for capability in &mut app.capabilities.execution_action_capabilities {
        capability
            .scopes
            .push(v2::ExecutionScopeKind::Session as i32);
    }
    let (tx, mut rx) = mpsc::channel(32);
    apply_message(
        &mut app,
        UiMessage::DebuggerEvent(model::DebuggerEvent {
            server_instance_id: "backend/native-ux".into(),
            state_revision: 2,
            state_event_sequence: 2,
            summary: "server breakpoint".into(),
            refresh: false,
            activity: DebuggerActivity::Stopped(Some("thread/b".into())),
        }),
        &tx,
    );
    assert_eq!(
        app.current_thread().unwrap().id,
        "thread/b",
        "the event's owner is still marked running in the previous snapshot"
    );
    let mut stopped = snapshot(&["a", "b"], 3);
    stopped.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Signal as i32,
        signal_name: Some("SIGINT".into()),
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    // Recovery can enumerate the automatic peer before the initiating server.
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    assert_eq!(app.selected_topology_thread_id(), Some("thread/b"));
    assert_eq!(app.pause_focus.as_deref(), Some("thread/b"));
    let queued = std::iter::from_fn(|| rx.try_recv().ok())
        .map(BackendRequest::untracked)
        .collect::<Vec<_>>();
    assert!(queued.iter().any(|request| matches!(request, BackendRequest::InspectThread { thread_id, .. } if thread_id == "thread/b")));
    assert!(!queued
        .iter()
        .any(|request| matches!(request, BackendRequest::Control(_, _))));
}

#[test]
fn snapshot_first_stop_batch_selects_the_principal_before_its_automatic_peer() {
    let mut app = app(&[]);
    let mut stopped = snapshot(&["a", "b"], 2);
    stopped.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Signal as i32,
        signal_name: Some("SIGINT".into()),
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    let (tx, _rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    assert_eq!(app.pause_focus.as_deref(), Some("thread/b"));
    assert_eq!(app.all_stop_cause.as_deref(), Some("thread/b"));
}

#[test]
fn authoritative_snapshot_replaces_provisional_automatic_peer_focus() {
    let mut app = app(&[]);
    app.note_automatic_pause("a");
    let (tx, _rx) = mpsc::channel(32);
    apply_message(
        &mut app,
        UiMessage::DebuggerEvent(model::DebuggerEvent {
            server_instance_id: "backend/native-ux".into(),
            state_revision: 2,
            state_event_sequence: 2,
            summary: "automatic peer interrupt".into(),
            refresh: false,
            activity: DebuggerActivity::Stopped(Some("thread/a".into())),
        }),
        &tx,
    );
    let mut stopped = snapshot(&["a", "b"], 3);
    stopped.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Signal as i32,
        signal_name: Some("SIGINT".into()),
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/b");
    assert_eq!(app.pause_focus.as_deref(), Some("thread/b"));
    assert_eq!(app.all_stop_cause.as_deref(), Some("thread/b"));
}

#[test]
fn independent_interrupt_remains_the_pause_owner_when_a_later_breakpoint_arrives() {
    let mut app = app(&[]);
    let mut interrupted = snapshot(&["a"], 2);
    interrupted.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Pause as i32,
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    let (tx, _rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(interrupted.clone()), &tx);
    assert_eq!(app.pause_focus.as_deref(), Some("thread/a"));
    let mut later = snapshot(&["a", "b"], 3);
    later.execution_states[0] = interrupted.execution_states[0].clone();
    apply_message(&mut app, UiMessage::Snapshot(later), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/a");
    assert_eq!(app.pause_focus.as_deref(), Some("thread/a"));
}

#[test]
fn process_wide_stop_replaces_an_old_owner_without_a_running_projection() {
    let mut app = app(&["a"]);
    app.select_pause_thread("thread/a");
    let mut stopped = snapshot(&["a"], 2);
    let mut rpc = stopped.threads[0].clone();
    rpc.thread_id = "thread/rpc".into();
    stopped.threads.push(rpc);
    let mut execution = stopped.execution_states[0].clone();
    execution.execution_state_id = "execution/rpc".into();
    execution.target = Some(api::thread_target("thread/rpc"));
    execution.stop_reason.as_mut().unwrap().thread_id = Some("thread/rpc".into());
    stopped.execution_states[0].stop_reason = execution.stop_reason.clone();
    stopped.execution_states.push(execution);
    let (tx, _rx) = mpsc::channel(32);
    apply_message(&mut app, UiMessage::Snapshot(stopped), &tx);
    assert_eq!(app.current_thread().unwrap().id, "thread/rpc");
    assert_eq!(app.pause_focus.as_deref(), Some("thread/rpc"));
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
        app.control_target(v2::ExecutionAction::Next),
        Some(api::thread_target("thread/b"))
    );
    app.frames[0].boundary = true;
    assert!(app.control_target(v2::ExecutionAction::Next).is_none());
    assert!(matches!(
        app.control_target(v2::ExecutionAction::Continue)
            .unwrap()
            .selector,
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
        app.control_target(v2::ExecutionAction::Continue),
        Some(api::thread_target("thread/a"))
    );
    for _ in 0..4 {
        app.cycle_execution_scope();
    }
    assert!(!app.execution_scope_override);
    assert_eq!(app.control_scope_label(), "Auto");
    assert!(matches!(
        app.control_target(v2::ExecutionAction::Continue)
            .unwrap()
            .selector,
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
fn breakpoint_rows_stay_in_place_when_frame_inspection_refreshes_snapshot_order() {
    let mut app = app(&["a", "b"]);
    let mut first = snapshot(&["a", "b"], 1);
    let mut second_breakpoint = first.breakpoints[0].clone();
    second_breakpoint.breakpoint_id = "breakpoint/other".into();
    first.breakpoints.push(second_breakpoint);
    first.breakpoints[0].sub_breakpoints = ["a", "b"]
        .map(|id| v2::SubBreakpoint {
            sub_breakpoint_id: format!("member/{id}"),
            session_id: id.into(),
            ..Default::default()
        })
        .into();
    app.apply_snapshot(first.clone());
    app.selected_breakpoint = 0;
    app.selected_breakpoint_site = Some(1);
    let rows = |app: &App| {
        app.breakpoint_order()
            .into_iter()
            .flat_map(|index| {
                let bp = &app.breakpoints[index];
                std::iter::once(bp.id.clone()).chain(
                    bp.sub_breakpoints
                        .iter()
                        .map(|site| site.sub_breakpoint_id.clone()),
                )
            })
            .collect::<Vec<_>>()
    };
    let initial_rows = rows(&app);
    app.apply_frames(
        ["a", "b"]
            .map(|owner| v2::Frame {
                frame_id: format!("frame/{owner}"),
                thread_id: format!("thread/{owner}"),
                ..Default::default()
            })
            .into(),
    );
    app.focus = Focus::Stack;
    let (tx, mut rx) = mpsc::channel(64);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &tx,
    );
    assert_eq!(app.current_frame().unwrap().thread_id, "thread/b");
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|request| {
        matches!(request.untracked(), BackendRequest::InspectFrame { frame_id, .. } if frame_id == "frame/b")
    }));
    // State-store snapshots are sets of resources; their vectors may be
    // enumerated differently after inspecting another distributed frame.
    first.breakpoints[0].sub_breakpoints.reverse();
    first.breakpoints.reverse();
    apply_message(&mut app, UiMessage::RefreshSnapshot(first.clone()), &tx);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        &tx,
    );
    assert_eq!(app.current_frame().unwrap().thread_id, "thread/a");
    first.breakpoints.reverse();
    apply_message(&mut app, UiMessage::Snapshot(first), &tx);
    assert_eq!(rows(&app), initial_rows);
    assert_eq!(app.current_breakpoint().unwrap().id, "breakpoint/shared");
    assert_eq!(
        app.current_breakpoint().unwrap().sub_breakpoints[app.selected_breakpoint_site.unwrap()]
            .sub_breakpoint_id,
        "member/b"
    );

    let mut added = snapshot(&["a", "b"], 1);
    added.breakpoints[0].sub_breakpoints = ["0-new", "b", "a"]
        .map(|id| v2::SubBreakpoint {
            sub_breakpoint_id: format!("member/{id}"),
            session_id: "a".into(),
            ..Default::default()
        })
        .into();
    let mut newcomer = added.breakpoints[0].clone();
    newcomer.breakpoint_id = "breakpoint/0-new".into();
    newcomer.sub_breakpoints.clear();
    let mut other = newcomer.clone();
    other.breakpoint_id = "breakpoint/other".into();
    added.breakpoints.extend([newcomer, other]);
    app.apply_snapshot(added);
    assert_eq!(
        rows(&app),
        [
            "breakpoint/shared",
            "member/a",
            "member/b",
            "member/0-new",
            "breakpoint/other",
            "breakpoint/0-new",
        ]
    );
    assert_eq!(app.selected_breakpoint_site, Some(1));
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

#[test]
fn breakpoint_member_badges_only_report_current_breakpoint_hits() {
    let mut app = app(&["a"]);
    let mut state = snapshot(&["a"], 1);
    state.breakpoints[0]
        .sub_breakpoints
        .push(v2::SubBreakpoint {
            sub_breakpoint_id: "member/a".into(),
            session_id: "a".into(),
            ..Default::default()
        });
    app.apply_snapshot(state.clone());
    assert!(render(&mut app).contains("[1 hit]"));
    for (kind, label) in [
        (v2::StopReasonKind::Step, "[stepped]"),
        (v2::StopReasonKind::Pause, "[paused]"),
        (v2::StopReasonKind::Signal, "[signal]"),
        (v2::StopReasonKind::Exception, "[exception]"),
        (v2::StopReasonKind::Entry, "[entry]"),
    ] {
        let mut next = state.clone();
        next.execution_states[0].stop_reason.as_mut().unwrap().kind = kind as i32;
        app.apply_snapshot(next);
        let screen = render(&mut app);
        assert!(!screen.contains(label), "{label}: {screen}");
        assert!(!screen.contains("[1 hit]"));
    }
    app.mark_running(Some("thread/a"));
    assert!(!render(&mut app).contains("[running]"));
    app.backend_unavailable("offline");
    assert!(!render(&mut app).contains("[offline]"));
}

#[test]
fn topology_marks_only_principal_stops_and_preserves_automatic_pause_provenance() {
    let mut app = app(&["a"]);
    app.note_automatic_pause("b");
    let mut stopped = snapshot(&["a", "b"], 2);
    stopped.execution_states[1].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Signal as i32,
        signal_name: Some("SIGINT".into()),
        thread_id: Some("thread/b".into()),
        ..Default::default()
    });
    app.apply_snapshot(stopped.clone());
    app.apply_threads(stopped.threads.clone());
    assert!(app
        .topology_stop_label(&app.threads[0])
        .unwrap()
        .contains("breakpoint hit"));
    assert!(
        app.topology_stop_label(&app.threads[1]).is_none(),
        "automatically interrupted peers are just stopped"
    );
    app.apply_snapshot(stopped.clone());
    assert!(
        app.topology_stop_label(&app.threads[1]).is_none(),
        "duplicate snapshots retain exact-stop provenance"
    );
    stopped.execution_states[1].revision = 3;
    stopped.execution_states[1]
        .stop_reason
        .as_mut()
        .unwrap()
        .signal_name = Some("SIGUSR1".into());
    app.apply_snapshot(stopped.clone());
    assert!(
        app.topology_stop_label(&app.threads[1])
            .unwrap()
            .contains("SIGUSR1"),
        "a later independent signal must not be hidden"
    );
    stopped.execution_states[1].revision = 4;
    stopped.execution_states[1]
        .stop_reason
        .as_mut()
        .unwrap()
        .thread_id = Some("thread/a".into());
    app.apply_snapshot(stopped);
    assert!(
        app.topology_stop_label(&app.threads[1]).is_none(),
        "process siblings do not become triggering threads"
    );
}

#[test]
fn automatic_pause_never_hides_concurrent_independent_breakpoint_or_signal() {
    for kind in [v2::StopReasonKind::Breakpoint, v2::StopReasonKind::Signal] {
        let mut app = app(&["a"]);
        app.note_automatic_pause("b");
        let mut stopped = snapshot(&["a", "b"], 2);
        let reason = stopped.execution_states[1].stop_reason.as_mut().unwrap();
        reason.kind = kind as i32;
        reason.signal_name = Some("SIGUSR1".into());
        app.apply_snapshot(stopped.clone());
        app.apply_threads(stopped.threads);
        assert!(app.topology_stop_label(&app.threads[1]).is_some());
    }
}

#[test]
fn session_only_topology_controls_selected_session_without_changing_toolbar_scope() {
    let mut app = app(&["a", "b"]);
    for capability in &mut app.capabilities.execution_action_capabilities {
        capability
            .scopes
            .push(v2::ExecutionScopeKind::Session as i32);
    }
    app.focus = Focus::Threads;
    app.select_visible_topology(&TopologyRowKind::Thread("thread/b".into()))
        .unwrap();
    let toolbar = app.control_target(v2::ExecutionAction::Continue);
    app.toggle_topology_sessions();
    assert!(app
        .topology_rows()
        .iter()
        .all(|row| !matches!(row.kind, TopologyRowKind::Thread(_))));
    assert_eq!(app.selected_topology_session_id().as_deref(), Some("b"));
    assert_eq!(app.control_target(v2::ExecutionAction::Continue), toolbar);
    let (tx, mut rx) = mpsc::channel(8);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE),
        &tx,
    );
    assert!(
        matches!(rx.try_recv().unwrap().untracked(), BackendRequest::Control(Control::Continue, target) if target == api::session_target("b"))
    );
    assert_eq!(app.control_target(v2::ExecutionAction::Continue), toolbar);
    let screen = render(&mut app);
    assert!(screen.contains("Signal [s]"));
    assert!(app.areas.topology_sessions_toggle.is_some());
    app.toggle_topology_sessions();
    assert!(app
        .topology_rows()
        .iter()
        .any(|row| matches!(row.kind, TopologyRowKind::Thread(_))));
}

#[test]
fn breakpoint_type_tabs_exclude_hidden_targets_and_clear_cross_type_selection() {
    let mut app = app(&["a"]);
    app.capabilities.breakpoint_features.extend([
        v2::BreakpointFeature::Function as i32,
        v2::BreakpointFeature::Distributed as i32,
        v2::BreakpointFeature::GroupInheritance as i32,
    ]);
    app.start_function_breakpoint_picker("main".into(), Default::default())
        .unwrap();
    let picker = app.breakpoint_target_picker.as_ref().unwrap();
    assert!(picker.visible_indices().iter().all(|index| !matches!(
        picker.choices[*index].target,
        api::BreakpointTarget::Group(_)
    )));
    app.toggle_breakpoint_target_choice();
    assert!(app
        .breakpoint_target_picker
        .as_ref()
        .unwrap()
        .choices
        .iter()
        .any(|choice| choice.selected));
    let (tx, mut rx) = mpsc::channel(8);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        &tx,
    );
    let picker = app.breakpoint_target_picker.as_ref().unwrap();
    assert!(picker.choices.iter().all(|choice| !choice.selected));
    assert!(picker.visible_indices().iter().all(|index| matches!(
        picker.choices[*index].target,
        api::BreakpointTarget::Group(_)
    )));
    let _ = render(&mut app);
    assert_eq!(app.areas.breakpoint_kind_tabs.len(), 2);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        &tx,
    );
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );
    assert!(
        matches!(rx.try_recv().unwrap().untracked(), BackendRequest::CreateBreakpoint { target: api::BreakpointTarget::Group(group), .. } if group == "workers")
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn inherited_member_deletion_confirms_the_group_rule_and_rejects_changed_scope() {
    let mut app = app(&["a", "b"]);
    app.breakpoints[0].target = Some(v2::Target {
        selector: Some(v2::target::Selector::Multiple(v2::MultipleTarget {
            targets: vec![api::session_target("a"), api::group_target("workers")],
        })),
    });
    app.breakpoints[0].sub_breakpoints.push(v2::SubBreakpoint {
        session_id: "b".into(),
        inherited_from_group_id: Some("workers".into()),
        ..Default::default()
    });
    app.selected_breakpoint_site = Some(0);
    app.focus = Focus::Breakpoints;
    let (tx, mut rx) = mpsc::channel(8);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        &tx,
    );
    assert!(
        rx.try_recv().is_err(),
        "Delete must first show the scope confirmation"
    );
    let screen = render(&mut app);
    assert!(screen.contains("current and future members"), "{screen}");
    assert!(screen.contains("Group rule"));
    assert!(screen.contains("Session rule"));
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &tx,
    );
    assert!(
        matches!(rx.try_recv().unwrap().untracked(), BackendRequest::DeleteBreakpoint { target, .. } if target == api::group_target("workers"))
    );
    app.open_breakpoint_deletion().unwrap();
    app.breakpoints[0].target = Some(api::session_target("a"));
    assert!(app
        .commit_breakpoint_deletion()
        .unwrap_err()
        .contains("changed"));
}

#[test]
fn pending_group_rules_are_manageable_without_installed_members() {
    let mut app = app(&[]);
    app.breakpoints[0].target = Some(api::group_target("workers"));
    assert_eq!(app.breakpoints[0].rule_kind(), "Group");
    assert!(app.breakpoints[0].sub_breakpoints.is_empty());
    app.open_breakpoint_deletion().unwrap();
    assert!(
        matches!(app.commit_breakpoint_deletion().unwrap(), BackendRequest::DeleteBreakpoint { target, .. } if target == api::group_target("workers"))
    );
}

#[test]
fn custom_session_signal_keeps_its_chosen_target_and_rejects_replaced_backend() {
    for replace in [false, true] {
        let mut app = app(&["a", "b"]);
        app.open_signal_picker(
            api::session_target("b"),
            vec![v2::DebuggerSignal {
                name: "SIGUSR1".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            &tx,
        );
        app.execution_target = Some(api::session_target("a"));
        app.insert_input("SIGUSR2");
        if replace {
            let mut replacement = snapshot(&["a", "b"], 1);
            replacement.server_instance_id = "replacement".into();
            app.apply_snapshot(replacement);
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        if replace {
            assert!(
                rx.try_recv().is_err(),
                "do not send an old prompt to a replacement backend"
            );
            assert!(app.status.contains("instance changed"));
        } else {
            assert!(
                matches!(rx.try_recv().unwrap().untracked(), BackendRequest::SendSignal { target, signal } if target == api::session_target("b") && signal == "SIGUSR2")
            );
        }
    }
}

#[test]
fn ctrl_p_and_ctrl_n_follow_arrow_navigation_in_every_non_source_panel() {
    let fixture = |focus| {
        let mut app = app(&["a", "b"]);
        app.focus = focus;
        app.expanded_pane = true;
        app.breakpoints.push(app.breakpoints[0].clone());
        app.breakpoints[1].id = "breakpoint/second".into();
        app.apply_frames(
            (0..2)
                .map(|level| v2::Frame {
                    frame_id: format!("frame/{level}"),
                    thread_id: "thread/a".into(),
                    level,
                    function_name: Some(format!("function_{level}")),
                    ..Default::default()
                })
                .collect(),
        );
        app.apply_variables(
            (0..2)
                .map(|index| v2::Variable {
                    variable_id: format!("variable/{index}"),
                    name: format!("value_{index}"),
                    ..Default::default()
                })
                .collect(),
        );
        app.apply_registers(
            (0..2)
                .map(|index| v2::Register {
                    name: format!("r{index}"),
                    value: index.to_string(),
                    ..Default::default()
                })
                .collect(),
        );
        app.apply_memory(v2::MemoryBlock {
            data: vec![0; 64],
            ..Default::default()
        });
        app.extension_panels.push(model::ExtensionPanelView {
            extension_title: "Test".into(),
            description: String::new(),
            panel_title: "Rows".into(),
            columns: vec!["Value".into()],
            rows: (0..100).map(|i| vec![i.to_string()]).collect(),
        });
        for i in 0..100 {
            app.push_timeline(format!("event {i}"));
            app.output.push(&format!("output {i}"));
        }
        render(&mut app);
        app.output.scroll(-4);
        app.timeline_scroll = 4;
        app.extension_scroll = 4;
        app
    };
    for focus in [
        Focus::Threads,
        Focus::Breakpoints,
        Focus::Stack,
        Focus::Variables,
        Focus::Registers,
        Focus::Memory,
        Focus::Extensions,
        Focus::Timeline,
        Focus::Output,
    ] {
        let mut arrows = fixture(focus);
        let mut aliases = fixture(focus);
        let (arrow_tx, _arrow_rx) = mpsc::channel(64);
        let (alias_tx, _alias_rx) = mpsc::channel(64);
        let before = render(&mut arrows);
        for (arrow, alias) in [(KeyCode::Down, 'n'), (KeyCode::Up, 'p')] {
            handle_key(
                &mut arrows,
                KeyEvent::new(arrow, KeyModifiers::NONE),
                &arrow_tx,
            );
            handle_key(
                &mut aliases,
                KeyEvent::new(KeyCode::Char(alias), KeyModifiers::CONTROL),
                &alias_tx,
            );
            assert_eq!(
                render(&mut aliases),
                render(&mut arrows),
                "{focus:?} Ctrl+{alias}"
            );
            assert!(!aliases.source_explorer.visible);
            if alias == 'n' {
                assert_ne!(
                    render(&mut arrows),
                    before,
                    "fixture must move in {focus:?}"
                );
            }
        }
    }
}

#[test]
fn source_hjkl_moves_the_document_and_ctrl_p_keeps_the_file_picker() {
    let mut app = app(&["a"]);
    app.focus = Focus::Source;
    let source = source_types::Source::Local(source_types::LocalSource {
        root: source_types::LocalRoot {
            name: "test".into(),
            debugger_path: None,
            session_tags: vec![],
        },
        path: "main.rs".into(),
        owner_hint: vec![],
        owner_instance: None,
    });
    let request = app.source_browser.open_source(source.clone());
    app.source_browser.apply(
        request.generation,
        source_types::Content {
            source,
            start_line: 1,
            line_count: 4,
            total_lines: 4,
            content: (0..4)
                .map(|_| "long source ".repeat(30))
                .collect::<Vec<_>>()
                .join("\n"),
        },
    );
    app.source_browser.set_cursor(2);
    app.source_browser.fit_horizontal(40);
    let (tx, _rx) = mpsc::channel(64);
    for (key, line, column) in [('j', 3, 0), ('k', 2, 0), ('l', 2, 8), ('h', 2, 0)] {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
            &tx,
        );
        let document = app.source_browser.document().unwrap();
        assert_eq!((document.cursor, document.column), (line, column), "{key}");
    }
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &tx,
    );
    assert!(app.source_explorer.visible);
    assert_eq!(app.source_browser.document().unwrap().cursor, 2);
}

#[test]
fn pause_reason_belongs_to_topology_not_the_breakpoint_panel() {
    let mut app = app(&["a"]);
    let mut state = snapshot(&["a"], 2);
    state.execution_states[0].stop_reason = Some(v2::StopReason {
        kind: v2::StopReasonKind::Signal as i32,
        signal_name: Some("SIGUSR1".into()),
        thread_id: Some("thread/a".into()),
        ..Default::default()
    });
    state.breakpoints[0]
        .sub_breakpoints
        .push(v2::SubBreakpoint {
            sub_breakpoint_id: "member/a".into(),
            session_id: "a".into(),
            ..Default::default()
        });
    app.apply_snapshot(state);
    app.focus = Focus::Breakpoints;
    app.expanded_pane = true;
    assert!(!render(&mut app).contains("[signal]"));
    app.focus = Focus::Threads;
    let topology = render(&mut app);
    assert!(topology.contains("signal SIGUSR1"), "{topology}");
    assert!(topology.contains("trigger"), "{topology}");
}

#[test]
fn pending_toolbar_action_is_visible_in_place_and_repeated_click_is_not_queued() {
    let mut app = app(&["a"]);
    let (tx, mut rx) = mpsc::channel(16);
    dispatch_control(&mut app, &tx, Control::Continue);
    render(&mut app);
    let (_, area) = app
        .areas
        .controls
        .iter()
        .find(|(control, _)| *control == Control::Continue)
        .unwrap();
    let area = *area;
    let screen = render(&mut app);
    let button = screen
        .lines()
        .nth(area.y as usize)
        .unwrap()
        .chars()
        .skip(area.x as usize)
        .take(area.width as usize)
        .collect::<String>();
    assert!(button.contains(app.operations.spinner()), "{button}");
    dispatch_control(&mut app, &tx, Control::Continue);
    for _ in 0..3 {
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
    }
    assert!(rx.try_recv().is_ok());
    assert!(
        rx.try_recv().is_err(),
        "do not submit the same pending action twice"
    );
}

#[test]
fn ctrl_navigation_uses_modal_rows_from_source_and_leaves_prompt_input_unchanged() {
    let mut app = app(&["a", "b"]);
    app.focus = Focus::Source;
    app.open_breakpoint_hits().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    for (key, cursor) in [('n', 1), ('p', 0)] {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL),
            &tx,
        );
        assert_eq!(app.breakpoint_hit_picker.as_ref().unwrap().cursor, cursor);
        assert!(!app.source_explorer.visible);
    }
    app.breakpoint_hit_picker = None;
    app.focus = Focus::Breakpoints;
    app.start_input(InputMode::Evaluate);
    app.input = "value".into();
    for key in ['n', 'p'] {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL),
            &tx,
        );
        assert_eq!(app.input, "value");
        assert_eq!(app.input_mode, InputMode::Evaluate);
        assert!(!app.source_explorer.visible);
    }
}
