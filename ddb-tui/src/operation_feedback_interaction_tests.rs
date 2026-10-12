use super::*;
use crate::operation_feedback::Update;

fn snapshot() -> v2::Snapshot {
    v2::Snapshot {
        server_instance_id: "backend/a".into(),
        sessions: vec![v2::Session {
            session_id: "session/a".into(),
            display_name: "worker-a".into(),
            ..Default::default()
        }],
        threads: vec![v2::Thread {
            thread_id: "thread/a".into(),
            session_id: "session/a".into(),
            state: v2::ThreadState::Stopped as i32,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn fixture() -> App {
    let mut app = App::new("http://localhost".into());
    app.apply_snapshot(snapshot());
    app.apply_threads(snapshot().threads);
    app.capabilities.execution_actions = vec![
        v2::ExecutionAction::Next as i32,
        v2::ExecutionAction::Continue as i32,
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

fn operation(state: v2::OperationState) -> v2::Operation {
    v2::Operation {
        operation_id: "operation/a".into(),
        state: state as i32,
        ..Default::default()
    }
}

#[test]
fn control_feedback_survives_snapshots_inspection_and_prompt_input_in_every_layout() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    dispatch_control(&mut app, &tx, Control::Next);
    let BackendRequest::Tracked { ticket, request } = rx.try_recv().unwrap() else {
        panic!("control must carry its intent identity")
    };
    assert!(
        matches!(*request, BackendRequest::Control(Control::Next, target) if target == api::thread_target("thread/a"))
    );
    assert!(render(&mut app, 140, 42).contains("queued · next"));
    for update in [
        Update::Started,
        Update::Admitted(operation(v2::OperationState::Accepted)),
    ] {
        apply_message(
            &mut app,
            UiMessage::OperationFeedback { ticket, update },
            &tx,
        );
    }
    app.apply_snapshot(snapshot());
    app.status = "loading another frame".into();
    for (width, height) in [(140, 42), (75, 24), (60, 15)] {
        assert!(render(&mut app, width, height).contains("pending · next"));
        app.input_mode = InputMode::Evaluate;
        assert!(render(&mut app, width, height).contains("pending · next"));
        app.input_mode = InputMode::Normal;
    }
    for (width, height) in [(1, 1), (8, 5)] {
        render(&mut app, width, height);
    }
    apply_message(
        &mut app,
        UiMessage::OperationFeedback {
            ticket,
            update: Update::Finished(operation(v2::OperationState::Completed)),
        },
        &tx,
    );
    app.apply_snapshot(snapshot());
    assert!(render(&mut app, 140, 42).contains("completed · next"));
    app.error("Source unavailable for selected frame");
    let small = render(&mut app, 75, 15);
    assert!(small.contains("completed · next"));
    assert!(small.contains("Source unavailable for selected frame"));
    assert!(app
        .timeline
        .iter()
        .all(|line| !line.contains("operation/a")));
    app.trace_api = true;
    assert!(render(&mut app, 140, 42).contains("operation/a"));
}

#[test]
fn older_completions_and_automatic_receipts_do_not_replace_newer_intent() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    dispatch_control(&mut app, &tx, Control::Next);
    let BackendRequest::Tracked { ticket, .. } = rx.try_recv().unwrap() else {
        panic!()
    };
    dispatch_control(&mut app, &tx, Control::Continue);
    assert!(matches!(
        rx.try_recv().unwrap(),
        BackendRequest::Tracked { ticket: 2, .. }
    ));
    apply_message(
        &mut app,
        UiMessage::OperationFeedback {
            ticket,
            update: Update::Finished(operation(v2::OperationState::Completed)),
        },
        &tx,
    );
    apply_message(
        &mut app,
        UiMessage::Receipt(
            "all-stop pause session/b".into(),
            operation(v2::OperationState::Completed),
        ),
        &tx,
    );
    assert!(render(&mut app, 140, 42).contains("queued · continue"));
    assert!(app.operations.display().unwrap().contains("all sessions"));
    app.backend_unavailable("offline");
    assert!(render(&mut app, 140, 42).contains("outcome unconfirmed"));
    let mut replacement = snapshot();
    replacement.server_instance_id = "backend/b".into();
    app.apply_snapshot(replacement);
    apply_message(
        &mut app,
        UiMessage::OperationFeedback {
            ticket: 2,
            update: Update::Finished(operation(v2::OperationState::Completed)),
        },
        &tx,
    );
    assert!(app
        .operations
        .display()
        .unwrap()
        .contains("outcome unconfirmed"));
}

#[test]
fn failed_queue_admission_and_closed_worker_are_not_reported_as_pending() {
    for closed in [false, true] {
        let mut app = fixture();
        let (tx, rx) = mpsc::channel(1);
        if closed {
            drop(rx);
        } else {
            tx.try_send(BackendRequest::Refresh).unwrap();
        }
        dispatch_control(&mut app, &tx, Control::Next);
        let text = app.operations.display().unwrap();
        assert!(text.contains("error · next"));
        assert!(text.contains("not sent to DDB"));
        assert!(!text.contains("1 pending"));
    }
}

#[test]
fn mutation_feedback_captures_its_owner_without_recording_expression_or_assignment_text() {
    let app = fixture();
    let label = BackendRequest::Evaluate {
        expression: "secret_token".into(),
        thread_id: "thread/remote".into(),
        frame_id: None,
    }
    .feedback_label(&app)
    .unwrap();
    assert!(label.contains("thread/remote"));
    assert!(!label.contains("secret_token"));
    assert!(BackendRequest::Refresh.feedback_label(&app).is_none());
    assert!(BackendRequest::InspectThread {
        thread_id: "thread/a".into(),
        generation: 1,
        local_session: None
    }
    .feedback_label(&app)
    .is_none());
}
