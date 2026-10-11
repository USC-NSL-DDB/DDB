use super::*;
use crate::{
    model::{frame_sources::State, StackFrame},
    source_browser::SourceRequest,
    source_types::{Content, LocalSource, Source},
};

fn fixture() -> App {
    let mut app = App::new("http://localhost".into());
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend".into(),
        sessions: ["a", "b"]
            .into_iter()
            .map(|id| v2::Session {
                session_id: id.into(),
                tag: id.into(),
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
                session_id: id.into(),
                state: v2::ThreadState::Stopped as i32,
                ..Default::default()
            })
            .collect(),
    );
    app.frames = ["a", "b"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| StackFrame {
            id: format!("frame/{id}"),
            distributed_index: index,
            session_id: id.into(),
            thread_id: format!("thread/{id}"),
            level: 0,
            address: "0x1000".into(),
            function: format!("work_{id}"),
            file: Some("/build/main.rs".into()),
            line: Some(1),
            boundary: false,
        })
        .collect();
    app.focus = Focus::Stack;
    app.expanded_pane = true;
    app
}

fn read(app: &mut App, tx: &mpsc::Sender<BackendRequest>, owner: &str) -> u64 {
    let request = app
        .source_browser
        .navigate(
            Some(api::thread_target(format!("thread/{owner}"))),
            v2::SourceLocation {
                path: Some("/build/main.rs".into()),
                line: 1,
                ..Default::default()
            },
            Navigation::Frame,
        )
        .unwrap();
    let generation = request.generation;
    queue_request(app, tx, BackendRequest::LoadSource(request));
    generation
}

fn content() -> Content {
    Content {
        source: Source::Ddb {
            instance: "backend".into(),
            file: v2::SourceFile {
                source_reference: "source/a".into(),
                debugger_path: Some("/build/main.rs".into()),
                owners: vec![v2::SourceOwner {
                    session_id: "a".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        },
        content: "fn work_a() {}".into(),
        start_line: 1,
        line_count: 1,
        total_lines: 1,
    }
}

fn state(app: &App, index: usize) -> State {
    app.frame_source_status(&app.frames[index]).state
}

#[test]
fn frame_read_evidence_is_owner_scoped_generation_checked_retryable_and_visible() {
    let mut app = fixture();
    let (tx, _rx) = mpsc::channel(8);
    assert_eq!(state(&app, 0), State::Unknown);
    assert_eq!(
        app.frame_source_status(&app.frames[0]).label(),
        " [source unchecked]"
    );
    let old = read(&mut app, &tx, "a");
    assert_eq!(state(&app, 0), State::Loading);
    let generation = read(&mut app, &tx, "a");
    app.begin_frame_source_read(&SourceRequest {
        generation: old,
        address: crate::source_types::Address::Resolve {
            instance: Some("backend".into()),
            target: Some(api::thread_target("thread/b")),
            location: v2::SourceLocation {
                path: Some("/build/main.rs".into()),
                ..Default::default()
            },
        },
        line: 1,
    });
    assert_eq!(
        state(&app, 1),
        State::Unknown,
        "stale registrations cannot replace the current read owner"
    );
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation: old,
            error: "stale failure".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Loading);
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "file is not readable".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Failed);
    assert_eq!(
        state(&app, 1),
        State::Unknown,
        "identical path in another session is independent"
    );
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 25)).unwrap();
    terminal
        .draw(|frame| app.areas = ui::draw(frame, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let row = (0..25)
        .find(|row| {
            (0..100)
                .map(|col| buffer[(col, *row)].symbol())
                .collect::<String>()
                .contains("work_a [source load failed]")
        })
        .unwrap();
    let line = (0..100)
        .map(|col| buffer[(col, row)].symbol())
        .collect::<String>();
    let column = line[..line.find("work_a").unwrap()].chars().count() as u16;
    assert_eq!(
        buffer[(column, row)].fg,
        ratatui::style::Color::Rgb(128, 128, 128)
    );
    assert!(line.contains("file is not readable"));
    let generation = read(&mut app, &tx, "a");
    assert_eq!(state(&app, 0), State::Loading);
    apply_message(
        &mut app,
        UiMessage::Source {
            generation,
            source: content(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Readable);
    assert_eq!(state(&app, 1), State::Unknown);
    assert_ne!(
        app.status, "file is not readable",
        "a recovered source must not leave its old error displayed"
    );
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "late duplicate".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Readable);
    let generation = read(&mut app, &tx, "a");
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "source error".into(),
        },
        &tx,
    );
    let generation = read(&mut app, &tx, "a");
    app.error("another operation failed");
    apply_message(
        &mut app,
        UiMessage::Source {
            generation,
            source: content(),
        },
        &tx,
    );
    assert_eq!(
        app.status, "another operation failed",
        "source recovery must not erase a different error"
    );
}

#[test]
fn cancelled_reads_and_invalid_locations_do_not_create_false_unavailability() {
    let mut app = fixture();
    let (tx, mut rx) = mpsc::channel(8);
    let generation = read(&mut app, &tx, "a");
    rx.try_recv().unwrap();
    app.source_browser.set_following(false);
    assert_eq!(state(&app, 0), State::Unknown);
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "cancelled".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Unknown);
    for line in [None, Some(0), Some(u64::from(u32::MAX) + 1)] {
        app.frames[0].line = line;
        assert_eq!(state(&app, 0), State::NoLocation);
        reveal_selected_frame(&mut app, &tx);
        assert!(app
            .source_browser
            .error()
            .unwrap()
            .contains("no usable file/line"));
        assert!(
            rx.try_recv().is_err(),
            "invalid locations must not trigger an unrelated source read"
        );
    }
    app.frames[0].line = Some(1);
    app.frames[0].file = Some(String::new());
    assert_eq!(state(&app, 0), State::NoLocation);
    for (width, height) in [(1, 1), (8, 5), (45, 14)] {
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| app.areas = ui::draw(frame, &mut app))
            .unwrap();
    }
}

#[test]
fn ambiguous_and_foreign_backend_source_failures_do_not_dim_unrelated_frames() {
    let mut app = fixture();
    let (tx, _rx) = mpsc::channel(8);
    let request = app
        .source_browser
        .navigate(
            Some(api::group_target("group")),
            v2::SourceLocation {
                path: Some("/build/main.rs".into()),
                line: 1,
                ..Default::default()
            },
            Navigation::Manual,
        )
        .unwrap();
    let generation = request.generation;
    queue_request(&mut app, &tx, BackendRequest::LoadSource(request));
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "ambiguous owners".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Unknown);
    assert_eq!(state(&app, 1), State::Unknown);
    let Source::Ddb { file, .. } = content().source else {
        unreachable!()
    };
    let request = app.source_browser.open_source(Source::Ddb {
        instance: "old backend".into(),
        file,
    });
    let generation = request.generation;
    queue_request(&mut app, &tx, BackendRequest::LoadSource(request));
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "old source handle".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Unknown);
}

#[test]
fn frame_source_evidence_is_retired_with_sessions_and_backend_instances() {
    let mut app = fixture();
    let (tx, _rx) = mpsc::channel(8);
    let generation = read(&mut app, &tx, "a");
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "missing file".into(),
        },
        &tx,
    );
    let frames = app.frames.clone();
    let sessions = app.sessions.clone();
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend".into(),
        ..Default::default()
    });
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "backend".into(),
        sessions: sessions.clone(),
        ..Default::default()
    });
    app.frames = frames.clone();
    assert_eq!(state(&app, 0), State::Unknown);
    let generation = read(&mut app, &tx, "a");
    app.apply_snapshot(v2::Snapshot {
        server_instance_id: "new backend".into(),
        sessions,
        ..Default::default()
    });
    app.frames = frames;
    apply_message(
        &mut app,
        UiMessage::SourceError {
            generation,
            error: "old backend failure".into(),
        },
        &tx,
    );
    assert_eq!(state(&app, 0), State::Unknown);
}

#[tokio::test]
async fn actual_missing_and_invalid_utf8_sources_mark_the_owner_then_recover_on_retry() {
    for invalid_utf8 in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.rs");
        if invalid_utf8 {
            std::fs::write(&source, [0xff, 0]).unwrap();
        }
        let config = directory.path().join("workspace.yaml");
        std::fs::write(&config, serde_json::to_vec(&serde_json::json!({"roots": [{"name": "checkout", "path": directory.path(), "debugger_path": "/build", "session_tags": ["a"]}]})).unwrap()).unwrap();
        let mut app = fixture();
        app.source_workspace =
            std::sync::Arc::new(source_workspace::Workspace::load(&config).unwrap());
        app.source_workspace.update_sessions(&app.sessions);
        let root = app.source_workspace.roots().remove(0);
        let local = Source::Local(LocalSource {
            root,
            path: "main.rs".into(),
            owner_hint: Vec::new(),
            owner_instance: Some("backend".into()),
        });
        let client = api::ApiClient::new(api::ClientConfig::new("http://127.0.0.1:1")).unwrap();
        let (tx, requests) = mpsc::channel(8);
        let (messages, mut rx) = mpsc::channel(8);
        let worker = tokio::spawn(worker::run(
            client,
            app.source_workspace.clone(),
            requests,
            messages,
        ));
        let request = app.source_browser.open_source(local.clone());
        queue_request(&mut app, &tx, BackendRequest::LoadSource(request));
        let failure = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(&failure, UiMessage::SourceError { .. }));
        apply_message(&mut app, failure, &tx);
        assert_eq!(state(&app, 0), State::Failed);
        assert_eq!(state(&app, 1), State::Unknown);
        std::fs::write(source, "fn work_a() {}\n").unwrap();
        let request = app.source_browser.open_source(local);
        queue_request(&mut app, &tx, BackendRequest::LoadSource(request));
        assert_eq!(state(&app, 0), State::Loading);
        let success = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(&success, UiMessage::Source { .. }));
        apply_message(&mut app, success, &tx);
        assert_eq!(state(&app, 0), State::Readable);
        drop(tx);
        worker.await.unwrap();
    }
}
