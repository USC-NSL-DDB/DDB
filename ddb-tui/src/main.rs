mod api;
mod cli;
mod legacy_v1;
mod model;
mod supervisor;
mod ui;
mod worker;

use std::{io, time::Duration};

use anyhow::{Context, Result};
use api::{
    thread_target, ApiClient, BreakpointTarget, CapabilitiesExt, ClientConfig, V2ApiClient,
    TUI_API_COMPATIBILITY,
};
use clap::Parser;
use cli::{ApiVersion, Args, Mode};
use crossterm::{
    cursor::Show,
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ddb_api_client::v2;
use futures_util::StreamExt;
use model::{
    App, BackendRequest, BreakpointOptions, Control, DebuggerActivity, EventStreamStatus, Focus,
    InputMode, UiMessage,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use supervisor::ManagedBackend;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mode = args.parse_mode()?;

    let mut managed = match &mode {
        Mode::Connect { .. } => None,
        _ => Some(ManagedBackend::start(&args, &mode).await?),
    };
    let (endpoint, token) = match (&mode, managed.as_ref()) {
        (Mode::Connect { api }, None) => (api.clone(), args.token.clone()),
        (_, Some(backend)) => (
            backend.endpoint().to_string(),
            Some(backend.control_token().to_string()),
        ),
        _ => unreachable!("mode and backend ownership must agree"),
    };

    let primary = run_frontend(
        &endpoint,
        token.as_deref(),
        args.api_version,
        args.refresh_ms,
        managed.as_ref(),
    )
    .await;
    let cleanup = match managed.as_mut() {
        Some(backend) => backend.shutdown().await,
        None => Ok(()),
    };
    combine_primary_and_cleanup(primary, cleanup)
}

async fn run_frontend(
    endpoint: &str,
    token: Option<&str>,
    api_version: ApiVersion,
    refresh_ms: u64,
    managed: Option<&ManagedBackend>,
) -> Result<()> {
    let client = negotiate_client(endpoint, token, api_version, managed).await?;
    let mut app = App::new(endpoint.to_string());
    if let Some(backend) = managed {
        app.push_timeline(format!(
            "managed DDB {} ready (instance {}, log {})",
            backend.backend_version(),
            backend.server_instance_id(),
            backend.log_path().display()
        ));
    } else {
        app.push_timeline("connected to externally owned DDB");
    }

    let mut terminal_session = TerminalSession::enter()?;
    let terminal_backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(terminal_backend)?;

    let result = run(&mut terminal, &mut app, client, refresh_ms).await;
    drop(terminal);
    let restoration = terminal_session.restore();
    match (result, restoration) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(restoration)) => {
            Err(error.context(format!("terminal restoration also failed: {restoration:#}")))
        }
    }
}

async fn negotiate_client(
    endpoint: &str,
    token: Option<&str>,
    api_version: ApiVersion,
    managed: Option<&ManagedBackend>,
) -> Result<ApiClient> {
    let mut client_config = ClientConfig::new(endpoint);
    if let Some(token) = token {
        client_config = client_config.with_bearer_token(token.to_string());
    }
    let v2_client = V2ApiClient::new(client_config)?;
    match v2_client.handshake().await {
        Ok((server, capabilities)) => {
            capabilities.validate_for_tui()?;
            if let Some(backend) = managed {
                if server.server_instance_id != backend.server_instance_id() {
                    anyhow::bail!(
                        "managed DDB identity changed during startup: report={}, API={}",
                        backend.server_instance_id(),
                        server.server_instance_id
                    );
                }
                if server.version != backend.backend_version() {
                    anyhow::bail!(
                        "ddb-tui {} supports {}; startup report identified DDB {} with APIs {:?}, but the endpoint reports DDB {} with APIs {:?} and schema {:?}. Reinstall the paired package and ensure no stale backend is being reused",
                        env!("CARGO_PKG_VERSION"),
                        TUI_API_COMPATIBILITY,
                        backend.backend_version(),
                        backend.api_versions(),
                        server.version,
                        server.api_versions,
                        capabilities.schema_version
                    );
                }
            }
            Ok(ApiClient::V2(v2_client))
        }
        Err(error) if api_version == ApiVersion::V2 && managed.is_none() && error.is_retryable() => {
            Ok(ApiClient::V2(v2_client))
        }
        Err(error)
            if api_version == ApiVersion::V1Fallback
                && managed.is_none()
                && error.is_api_version_unavailable() =>
        {
            let client = legacy_v1::Client::new(endpoint, token)?;
            client.handshake().await.with_context(|| {
                format!("v2 is unavailable and the explicit v1 fallback failed: {error}")
            })?;
            Ok(ApiClient::V1Fallback(client))
        }
        Err(error) => Err(error).context(format!(
            "DDB API v2 negotiation failed; ddb-tui {} supports {}. For a version mismatch, install the paired package or choose a compatible frontend; refusing to downgrade for this error",
            env!("CARGO_PKG_VERSION"),
            TUI_API_COMPATIBILITY
        )),
    }
}

fn combine_primary_and_cleanup(primary: Result<()>, cleanup: Result<()>) -> Result<()> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(cleanup.context("failed to stop managed DDB")),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("managed DDB cleanup also failed: {cleanup:#}")))
        }
    }
}

struct TerminalSession {
    active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        let mut session = Self { active: true };
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
            let _ = session.restore();
            return Err(error).context("failed to enter the interactive terminal session");
        }
        Ok(session)
    }

    fn restore(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let raw_result = disable_raw_mode().context("failed to disable terminal raw mode");
        let screen_result = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        )
        .context("failed to restore the terminal screen");
        raw_result.and(screen_result)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    client: ApiClient,
    refresh_ms: u64,
) -> Result<()> {
    let (request_tx, request_rx) = mpsc::channel(64);
    let (message_tx, mut message_rx) = mpsc::channel(128);
    let mut worker_task = tokio::spawn(worker::run(client.clone(), request_rx, message_tx.clone()));
    let mut event_task = tokio::spawn(worker::watch_events(client.clone(), message_tx.clone()));
    let mut output_task = tokio::spawn(worker::watch_output(client, message_tx));
    request_tx
        .send(BackendRequest::Bootstrap)
        .await
        .context("backend worker stopped")?;

    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_millis(refresh_ms.max(250)));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut termination = Box::pin(termination_signal());

    let mut worker_finished = false;
    let mut event_finished = false;
    let mut output_finished = false;
    let mut background_failure = None;
    while !app.should_quit {
        terminal.draw(|frame| {
            app.areas = ui::draw(frame, app);
        })?;

        tokio::select! {
            signal = &mut termination => {
                if let Err(error) = signal {
                    background_failure = Some(error);
                }
                app.should_quit = true;
            }
            event = events.next() => {
                match event {
                    Some(Ok(event)) => handle_event(app, event, &request_tx),
                    Some(Err(error)) => app.error(format!("terminal event error: {error}")),
                    None => app.should_quit = true,
                }
            }
            message = message_rx.recv() => {
                let Some(message) = message else {
                    app.error("backend worker stopped");
                    app.should_quit = true;
                    continue;
                };
                apply_message(app, message, &request_tx);
            }
            _ = refresh.tick() => {
                queue_recovery(app, &request_tx);
            }
            result = &mut worker_task, if !worker_finished => {
                worker_finished = true;
                background_failure = Some(background_task_error("backend worker", result));
                app.should_quit = true;
            }
            result = &mut event_task, if !event_finished => {
                event_finished = true;
                background_failure = Some(background_task_error("event worker", result));
                app.should_quit = true;
            }
            result = &mut output_task, if !output_finished => {
                output_finished = true;
                background_failure = Some(background_task_error("output worker", result));
                app.should_quit = true;
            }
        }
    }
    if !worker_finished {
        worker_task.abort();
        let _ = worker_task.await;
    }
    if !event_finished {
        event_task.abort();
        let _ = event_task.await;
    }
    if !output_finished {
        output_task.abort();
        let _ = output_task.await;
    }
    background_failure.map_or(Ok(()), Err)
}

#[cfg(unix)]
async fn termination_signal() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut hangup = signal(SignalKind::hangup()).context("failed to register SIGHUP handler")?;
    let mut interrupt =
        signal(SignalKind::interrupt()).context("failed to register SIGINT handler")?;
    let mut terminate =
        signal(SignalKind::terminate()).context("failed to register SIGTERM handler")?;
    tokio::select! {
        _ = hangup.recv() => {}
        _ = interrupt.recv() => {}
        _ = terminate.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn termination_signal() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("failed to wait for termination signal")
}

fn background_task_error(name: &str, result: Result<(), tokio::task::JoinError>) -> anyhow::Error {
    match result {
        Ok(()) => anyhow::anyhow!("{name} stopped unexpectedly"),
        Err(error) => anyhow::anyhow!("{name} failed: {error}"),
    }
}

fn handle_event(app: &mut App, event: Event, requests: &mpsc::Sender<BackendRequest>) {
    #[cfg(debug_assertions)]
    if std::env::var_os("DDB_TUI_TEST_PANIC_ON_KEY").is_some()
        && matches!(
            &event,
            Event::Key(key)
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('P')
        )
    {
        panic!("intentional ddb-tui panic fault injection");
    }
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key, requests),
        Event::Mouse(mouse) => handle_mouse(app, mouse, requests),
        Event::Paste(text) if app.input_mode != InputMode::Normal => app.insert_input(&text),
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
        _ => {}
    }
}

fn handle_key(app: &mut App, key: KeyEvent, requests: &mpsc::Sender<BackendRequest>) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    if app.show_help {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
        ) {
            app.show_help = false;
        }
        return;
    }
    if app.breakpoint_target_picker.is_some() {
        handle_breakpoint_target_key(app, key, requests);
        return;
    }
    if app.signal_picker.is_some() {
        handle_signal_picker_key(app, key, requests);
        return;
    }
    if app.input_mode != InputMode::Normal {
        handle_prompt_key(app, key, requests);
        return;
    }

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => app.show_help = true,
        KeyCode::Char(':') => {
            app.start_input(InputMode::Command);
        }
        KeyCode::Char('e') => {
            if app.capabilities.supports_inspection("evaluate") {
                app.start_input(InputMode::Evaluate);
            } else {
                app.status = "the connected DDB backend does not support evaluation".to_string();
            }
        }
        KeyCode::Char('m') => {
            if app.capabilities.supports_inspection("memory") {
                app.start_input(InputMode::Memory);
            } else {
                app.status = "the connected DDB backend does not support memory reads".to_string();
            }
        }
        KeyCode::Char('r') => queue_recovery(app, requests),
        KeyCode::Char('c') => app.cycle_execution_scope(),
        KeyCode::Char('a') if app.focus == Focus::Extensions => {
            if let Err(error) = app.start_extension_action_input() {
                app.error(error);
            }
        }
        KeyCode::Char('b') => toggle_breakpoint(app, requests),
        KeyCode::Char('B') => start_breakpoint_prompt(app),
        KeyCode::Char('j') => start_execution_prompt(app, "jump", InputMode::Jump),
        KeyCode::Char('s') => request_signal_picker(app, requests),
        KeyCode::Char('g') => {
            if app.current_thread().is_some() {
                app.start_input(InputMode::GotoLine);
            } else {
                app.status = "select a DDB thread before opening source".to_string();
            }
        }
        KeyCode::Char('d') => refresh_stack(app, requests),
        KeyCode::Delete | KeyCode::Backspace if app.focus == Focus::Breakpoints => {
            if !app.capabilities.supports_breakpoint_action("delete") {
                app.status = "breakpoint deletion is not supported by this DDB API".to_string();
            } else if let Some((id, target)) = app
                .current_breakpoint()
                .and_then(|breakpoint| Some((breakpoint.id.clone(), breakpoint.target.clone()?)))
            {
                queue_request(
                    app,
                    requests,
                    BackendRequest::DeleteBreakpoint { id, target },
                );
            }
        }
        KeyCode::Char('x') | KeyCode::Char(' ') if app.focus == Focus::Breakpoints => {
            toggle_breakpoint_enabled(app, requests)
        }
        KeyCode::Tab => {
            app.cycle_focus(key.modifiers.contains(KeyModifiers::SHIFT));
        }
        KeyCode::BackTab => app.cycle_focus(true),
        KeyCode::Up => {
            app.move_selection(-1);
            inspect_selection(app, requests);
        }
        KeyCode::Down => {
            app.move_selection(1);
            inspect_selection(app, requests);
        }
        KeyCode::PageUp => {
            app.move_selection(-10);
            inspect_selection(app, requests);
        }
        KeyCode::PageDown => {
            app.move_selection(10);
            inspect_selection(app, requests);
        }
        KeyCode::Enter if app.focus == Focus::Extensions => {
            if let Err(error) = app.start_extension_action_input() {
                app.error(error);
            }
        }
        KeyCode::Enter | KeyCode::Right if app.focus == Focus::Variables => {
            toggle_selected_variable(app, requests)
        }
        KeyCode::Enter => inspect_selection(app, requests),
        KeyCode::F(5) => dispatch_control(app, requests, Control::Continue),
        KeyCode::F(6) => dispatch_control(app, requests, Control::Interrupt),
        KeyCode::F(10) => dispatch_control(app, requests, Control::Next),
        KeyCode::F(11) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            dispatch_control(app, requests, Control::StepOut)
        }
        KeyCode::F(11) => dispatch_control(app, requests, Control::StepIn),
        _ => {}
    }
}

fn handle_breakpoint_target_key(
    app: &mut App,
    key: KeyEvent,
    requests: &mpsc::Sender<BackendRequest>,
) {
    match key.code {
        KeyCode::Esc => app.cancel_breakpoint_target_picker(),
        KeyCode::Up => app.move_breakpoint_target_picker(-1),
        KeyCode::Down => app.move_breakpoint_target_picker(1),
        KeyCode::PageUp => app.move_breakpoint_target_picker(-5),
        KeyCode::PageDown => app.move_breakpoint_target_picker(5),
        KeyCode::Char(' ') => app.toggle_breakpoint_target_choice(),
        KeyCode::Enter => submit_breakpoint_target_picker(app, requests),
        _ => {}
    }
}

fn submit_breakpoint_target_picker(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    match app.commit_breakpoint_target_picker() {
        Ok((draft, target)) => queue_request(
            app,
            requests,
            BackendRequest::CreateBreakpoint {
                source: draft.source,
                line: draft.line,
                target,
                options: draft.options,
            },
        ),
        Err(error) => app.error(error),
    }
}

fn handle_prompt_key(app: &mut App, key: KeyEvent, requests: &mpsc::Sender<BackendRequest>) {
    match key.code {
        KeyCode::Esc => app.cancel_input(),
        KeyCode::Enter => {
            let mode = app.input_mode;
            let input = app.commit_input();
            if !input.is_empty() || mode == InputMode::Breakpoint {
                match mode {
                    InputMode::Command => {
                        if is_raw_breakpoint_insert(&input) {
                            app.status = "use b/B for typed DDB breakpoint targeting".to_string();
                            return;
                        }
                        if let Some(target) = app
                            .current_thread()
                            .map(|thread| thread_target(thread.id.clone()))
                        {
                            queue_request(
                                app,
                                requests,
                                BackendRequest::RawCommand {
                                    command: input,
                                    target,
                                },
                            );
                        } else {
                            app.status =
                                "select a thread before sending a raw DDB/MI command".to_string();
                        }
                    }
                    InputMode::Evaluate => {
                        if let Some(thread_id) =
                            app.current_thread().map(|thread| thread.id.clone())
                        {
                            let frame_id = app.current_frame().map(|frame| frame.id.clone());
                            queue_request(
                                app,
                                requests,
                                BackendRequest::Evaluate {
                                    expression: input,
                                    thread_id,
                                    frame_id,
                                },
                            );
                        } else {
                            app.status = "select a thread before evaluating".to_string();
                        }
                    }
                    InputMode::Memory => {
                        if let Some(thread_id) =
                            app.current_thread().map(|thread| thread.id.clone())
                        {
                            match parse_memory_request(&input) {
                                Ok((address, count)) => queue_request(
                                    app,
                                    requests,
                                    BackendRequest::ReadMemory {
                                        address,
                                        count,
                                        thread_id,
                                        generation: app.inspection_generation,
                                    },
                                ),
                                Err(error) => app.error(error),
                            }
                        } else {
                            app.status = "select a thread before reading memory".to_string();
                        }
                    }
                    InputMode::Jump => {
                        queue_execution_operation(app, requests, "jump", |target| {
                            BackendRequest::Jump {
                                location: input,
                                target,
                            }
                        });
                    }
                    InputMode::Signal => {
                        queue_execution_operation(app, requests, "send_signal", |target| {
                            BackendRequest::SendSignal {
                                signal: input,
                                target,
                            }
                        });
                    }
                    InputMode::GotoLine => goto_source_line(app, requests, &input),
                    InputMode::Breakpoint => match parse_breakpoint_options(&input) {
                        Ok(options) => create_breakpoint(app, requests, options),
                        Err(error) => app.error(error),
                    },
                    InputMode::ExtensionAction => {
                        let payload = match serde_json::from_str::<serde_json::Value>(&input) {
                            Ok(payload) => payload,
                            Err(error) => {
                                app.error(format!(
                                    "extension action payload is not valid JSON: {error}"
                                ));
                                return;
                            }
                        };
                        let Some(action) = app.pending_extension_action.take() else {
                            app.error("the selected extension action is no longer available");
                            return;
                        };
                        let Some(target) = app.execution_target() else {
                            app.error(
                                "select a DDB execution target before invoking an extension action",
                            );
                            return;
                        };
                        queue_request(
                            app,
                            requests,
                            BackendRequest::InvokeExtensionAction {
                                extension_id: action.extension_id,
                                extension_version: action.extension_version,
                                action_id: action.action_id,
                                request_schema_uri: action.request_schema_uri,
                                payload_json: payload.to_string(),
                                target,
                            },
                        );
                    }
                    InputMode::Normal => {}
                }
            }
        }
        KeyCode::Backspace => app.backspace_input(),
        KeyCode::Delete => app.delete_input(),
        KeyCode::Left => app.move_input_cursor(-1),
        KeyCode::Right => app.move_input_cursor(1),
        KeyCode::Home => app.set_input_cursor(0),
        KeyCode::End => app.set_input_cursor(app.input.chars().count()),
        KeyCode::Up => app.previous_input(),
        KeyCode::Down => app.next_input(),
        KeyCode::Char(character)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            app.insert_input(&character.to_string());
        }
        _ => {}
    }
}

/// DDB's MI facade treats logical breakpoint insertion as a session/group
/// operation. Other raw commands retain the selected thread as their precise
/// execution context.
fn is_raw_breakpoint_insert(command: &str) -> bool {
    command
        .trim_start_matches(|character: char| character.is_ascii_digit())
        .split_whitespace()
        .next()
        == Some("-break-insert")
}

fn request_signal_picker(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    let action = "send_signal";
    if !app.capabilities.supports_execution(action) {
        app.status = "the connected DDB backend does not support send_signal".to_string();
        return;
    }
    let Some(target) = app.execution_target() else {
        app.status = "select a DDB thread or session before sending a signal".to_string();
        return;
    };
    if !app.capabilities.supports_execution_target(action, &target) {
        app.status = format!(
            "send_signal is not available for {}",
            app.execution_scope_label()
        );
        return;
    }
    if app.api_protocol.starts_with("v1/") {
        app.start_input(InputMode::Signal);
        return;
    }
    app.status = format!("loading signals for {}…", app.execution_scope_label());
    queue_request(app, requests, BackendRequest::ListSignals { target });
}

fn handle_signal_picker_key(app: &mut App, key: KeyEvent, requests: &mpsc::Sender<BackendRequest>) {
    match key.code {
        KeyCode::Esc => app.cancel_signal_picker(),
        KeyCode::Up => app.move_signal_picker(-1),
        KeyCode::Down => app.move_signal_picker(1),
        KeyCode::PageUp => app.move_signal_picker(-10),
        KeyCode::PageDown => app.move_signal_picker(10),
        KeyCode::Char('f') => {
            app.cancel_signal_picker();
            app.start_input(InputMode::Signal);
        }
        KeyCode::Enter => submit_signal_picker(app, requests),
        _ => {}
    }
}

fn submit_signal_picker(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    match app.commit_signal_picker() {
        Ok((signal, target)) => {
            queue_request(app, requests, BackendRequest::SendSignal { signal, target });
        }
        Err(error) => app.error(error),
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, requests: &mpsc::Sender<BackendRequest>) {
    if app.breakpoint_target_picker.is_some() {
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some(index) = app.areas.breakpoint_target_at(mouse.column, mouse.row) {
                    app.select_breakpoint_target_choice(index);
                } else if app
                    .areas
                    .breakpoint_target_apply_at(mouse.column, mouse.row)
                {
                    submit_breakpoint_target_picker(app, requests);
                } else if app
                    .areas
                    .breakpoint_target_cancel_at(mouse.column, mouse.row)
                {
                    app.cancel_breakpoint_target_picker();
                }
            }
            MouseEventKind::ScrollUp => app.move_breakpoint_target_picker(-3),
            MouseEventKind::ScrollDown => app.move_breakpoint_target_picker(3),
            _ => {}
        }
        return;
    }
    if app.signal_picker.is_some() {
        match mouse.kind {
            MouseEventKind::Down(_) => {
                if let Some(index) = app.areas.signal_at(mouse.column, mouse.row) {
                    app.select_signal(index);
                    submit_signal_picker(app, requests);
                } else if app.areas.signal_cancel_at(mouse.column, mouse.row) {
                    app.cancel_signal_picker();
                }
            }
            MouseEventKind::ScrollUp => app.move_signal_picker(-3),
            MouseEventKind::ScrollDown => app.move_signal_picker(3),
            _ => {}
        }
        return;
    }

    match mouse.kind {
        MouseEventKind::Down(button) => {
            if let Some(control) = app.areas.control_at(mouse.column, mouse.row) {
                match control {
                    Control::Refresh => queue_recovery(app, requests),
                    Control::CycleScope => app.cycle_execution_scope(),
                    Control::RefreshStack => refresh_stack(app, requests),
                    other => dispatch_control(app, requests, other),
                }
                return;
            }
            if let Some(index) = app.areas.extension_action_at(mouse.column, mouse.row) {
                app.focus = Focus::Extensions;
                app.selected_extension_action =
                    index.min(app.extension_actions.len().saturating_sub(1));
                if button == MouseButton::Left {
                    if let Err(error) = app.start_extension_action_input() {
                        app.error(error);
                    }
                }
                return;
            }
            if let Some(index) = app.areas.breakpoint_at(mouse.column, mouse.row) {
                app.focus = Focus::Breakpoints;
                app.selected_breakpoint = index.min(app.breakpoints.len().saturating_sub(1));
                return;
            }
            if let Some((focus, item)) = app.areas.item_at(mouse.column, mouse.row) {
                app.focus = focus;
                let selected = select_mouse_item(app, focus, item);
                if focus == Focus::Variables && button == MouseButton::Right {
                    toggle_selected_variable(app, requests);
                } else if selected {
                    inspect_selection(app, requests);
                }
            } else if let Some(focus) = app.areas.panel_at(mouse.column, mouse.row) {
                app.focus = focus;
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(focus) = app.areas.panel_at(mouse.column, mouse.row) {
                app.focus = focus;
            }
            app.move_selection(-3);
            inspect_selection(app, requests);
        }
        MouseEventKind::ScrollDown => {
            if let Some(focus) = app.areas.panel_at(mouse.column, mouse.row) {
                app.focus = focus;
            }
            app.move_selection(3);
            inspect_selection(app, requests);
        }
        _ => {}
    }
}

fn select_mouse_item(app: &mut App, focus: Focus, item: usize) -> bool {
    match focus {
        Focus::Threads => app.select_topology_row(item),
        Focus::Breakpoints => {
            app.selected_breakpoint = item.min(app.breakpoints.len().saturating_sub(1));
            true
        }
        Focus::Stack => {
            app.selected_frame = item.min(app.frames.len().saturating_sub(1));
            true
        }
        Focus::Source => {
            if let Some(source) = app.source.as_ref() {
                let line = (source.start_line + item).min(source.end_line);
                app.set_source_cursor_line(line);
            }
            true
        }
        Focus::Timeline => {
            app.timeline_scroll = item.min(app.timeline.len().saturating_sub(1));
            true
        }
        Focus::Extensions => {
            app.extension_scroll = item;
            true
        }
        Focus::Variables => {
            app.selected_variable = item.min(app.variables.len().saturating_sub(1));
            true
        }
    }
}

fn toggle_selected_variable(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    let Some(thread_id) = app.current_thread().map(|thread| thread.id.clone()) else {
        return;
    };
    let generation = app.inspection_generation;
    if let Some(variable_id) = app.toggle_selected_variable() {
        queue_request(
            app,
            requests,
            BackendRequest::ExpandVariable {
                variable_id,
                thread_id,
                generation,
            },
        );
    }
}

fn inspect_selection(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    match app.focus {
        Focus::Threads => {
            if let Some(thread_id) = app.selected_topology_thread_id().map(str::to_string) {
                inspect_thread(app, requests, thread_id);
            }
        }
        Focus::Stack => {
            if let (Some(thread_id), Some(frame)) = (
                app.current_thread().map(|thread| thread.id.clone()),
                app.current_frame().cloned(),
            ) {
                if frame.boundary {
                    app.status = frame.function;
                    return;
                }
                let generation = app.begin_frame_inspection();
                queue_request(
                    app,
                    requests,
                    BackendRequest::InspectFrame {
                        thread_id,
                        owner_thread_id: frame.thread_id,
                        generation,
                        frame_id: frame.id,
                        source: frame.file,
                        line: frame.line,
                    },
                );
            }
        }
        _ => {}
    }
}

fn inspect_thread(app: &mut App, requests: &mpsc::Sender<BackendRequest>, thread_id: String) {
    if let Some(index) = app.threads.iter().position(|thread| thread.id == thread_id) {
        app.selected_thread = index;
    } else {
        return;
    }
    let generation = app.begin_inspection();
    app.status = format!("loading thread #{thread_id}…");
    queue_request(
        app,
        requests,
        BackendRequest::InspectThread {
            thread_id,
            generation,
        },
    );
}

fn dispatch_control(app: &mut App, requests: &mpsc::Sender<BackendRequest>, control: Control) {
    let Some(action) = control.action_name() else {
        return;
    };
    if !app.capabilities.supports_execution(action) {
        app.status = format!("the connected DDB backend does not support {action}");
        return;
    }
    let Some(target) = app.execution_target() else {
        app.status = "select a DDB thread, session, or group before using controls".to_string();
        return;
    };
    if !app.capabilities.supports_execution_target(action, &target) {
        app.status = format!(
            "{action} is not available for {}",
            app.execution_scope_label()
        );
        return;
    }
    let scope = app.execution_scope_label();
    app.status = format!("sending {action} to {scope}…");
    queue_request(app, requests, BackendRequest::Control(control, target));
}

fn start_execution_prompt(app: &mut App, action: &str, mode: InputMode) {
    if !app.capabilities.supports_execution(action) {
        app.status = format!("the connected DDB backend does not support {action}");
        return;
    }
    let Some(target) = app.execution_target() else {
        app.status = "select a DDB thread or session before using this operation".to_string();
        return;
    };
    if !app.capabilities.supports_execution_target(action, &target) {
        app.status = format!(
            "{action} is not available for {}",
            app.execution_scope_label()
        );
        return;
    }
    app.start_input(mode);
}

fn refresh_stack(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    if !app
        .capabilities
        .supports_ddb_feature("distributed_backtrace")
    {
        app.status = "DDB distributed stack is not available for this runtime".to_string();
        return;
    }
    let Some(thread_id) = app.current_thread().map(|thread| thread.id.clone()) else {
        app.status = "select a thread before refreshing the DDB stack".to_string();
        return;
    };
    inspect_thread(app, requests, thread_id);
}

fn toggle_breakpoint(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    let Some(session_id) = app.current_thread().map(|thread| thread.session_id.clone()) else {
        app.status = "select a DDB session before setting a breakpoint".to_string();
        return;
    };
    let target = BreakpointTarget::Session(session_id);
    if let Some((id, api_target)) = app
        .breakpoint_at_source_cursor(target)
        .and_then(|breakpoint| Some((breakpoint.id.clone(), breakpoint.target.clone()?)))
    {
        if !app.capabilities.supports_breakpoint_action("delete") {
            app.status = "breakpoint deletion is not supported by this DDB API".to_string();
            return;
        }
        queue_request(
            app,
            requests,
            BackendRequest::DeleteBreakpoint {
                id,
                target: api_target,
            },
        );
        return;
    }
    if let Err(error) = app.start_breakpoint_target_picker(BreakpointOptions::default()) {
        app.error(error);
    }
}

fn start_breakpoint_prompt(app: &mut App) {
    if !app.capabilities.supports_breakpoint_action("create") {
        app.status = "source breakpoint creation is not supported by this DDB API".to_string();
    } else if !["conditional", "temporary", "hardware"]
        .into_iter()
        .any(|feature| app.capabilities.supports_breakpoint_action(feature))
    {
        app.status = "advanced breakpoints are not supported by this DDB API".to_string();
    } else if app.source_cursor_location().is_none() {
        app.status = "load a source file before setting a breakpoint".to_string();
    } else if app.current_thread().is_none() {
        app.status = "select a DDB session before setting a breakpoint".to_string();
    } else {
        app.start_input(InputMode::Breakpoint);
    }
}

fn toggle_breakpoint_enabled(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    if !app.capabilities.supports_breakpoint_action("enable")
        || !app.capabilities.supports_breakpoint_action("disable")
    {
        app.status = "breakpoint enable/disable is not supported by this DDB API".to_string();
        return;
    }
    let Some((id, target, enabled)) = app.current_breakpoint().and_then(|breakpoint| {
        Some((
            breakpoint.id.clone(),
            breakpoint.target.clone()?,
            !breakpoint.enabled,
        ))
    }) else {
        app.status = "select a breakpoint before enabling or disabling it".to_string();
        return;
    };
    queue_request(
        app,
        requests,
        BackendRequest::SetBreakpointEnabled {
            id,
            target,
            enabled,
        },
    );
}

fn create_breakpoint(
    app: &mut App,
    _requests: &mpsc::Sender<BackendRequest>,
    options: BreakpointOptions,
) {
    if let Err(error) = app.start_breakpoint_target_picker(options) {
        app.error(error);
    }
}

fn queue_execution_operation(
    app: &mut App,
    requests: &mpsc::Sender<BackendRequest>,
    action: &str,
    build: impl FnOnce(v2::Target) -> BackendRequest,
) {
    let Some(target) = app.execution_target() else {
        app.status = "select a DDB thread or session before using this operation".to_string();
        return;
    };
    if !app.capabilities.supports_execution_target(action, &target) {
        app.status = format!(
            "{action} is not available for {}",
            app.execution_scope_label()
        );
        return;
    }
    queue_request(app, requests, build(target));
}

fn goto_source_line(app: &mut App, requests: &mpsc::Sender<BackendRequest>, input: &str) {
    let (requested_source, line) = match parse_source_navigation(input) {
        Ok(location) => location,
        Err(error) => return app.error(error),
    };
    let Some(thread_id) = app.current_thread().map(|thread| thread.id.clone()) else {
        app.status = "select a thread before loading source".to_string();
        return;
    };
    let Some((source, line, generation)) = app.prepare_source_navigation(requested_source, line)
    else {
        app.status = "enter path:line when no source file is loaded".to_string();
        return;
    };
    queue_request(
        app,
        requests,
        BackendRequest::LoadSource {
            thread_id,
            generation,
            source,
            line,
        },
    );
}

fn parse_source_navigation(input: &str) -> Result<(Option<String>, usize), String> {
    let input = input.trim();
    let (source, line) = match input.parse::<usize>() {
        Ok(line) => (None, line),
        Err(_) => {
            let Some((source, line)) = input.rsplit_once(':') else {
                return Err("enter a line number or source path:line".to_string());
            };
            let source = source.trim();
            if source.is_empty() {
                return Err("source path must not be empty".to_string());
            }
            let line = line
                .trim()
                .parse::<usize>()
                .map_err(|_| "source line must be a positive integer".to_string())?;
            (Some(source.to_string()), line)
        }
    };
    if line == 0 {
        return Err("source line must be greater than zero".to_string());
    }
    Ok((source, line))
}

fn parse_memory_request(input: &str) -> Result<(String, u64), String> {
    let (address, count) = input
        .rsplit_once(';')
        .map(|(address, count)| (address.trim(), Some(count.trim())))
        .unwrap_or((input.trim(), None));
    if address.is_empty() {
        return Err("memory address must not be empty".to_string());
    }
    let count = count
        .filter(|count| !count.is_empty())
        .map(|count| {
            count
                .parse::<u64>()
                .map_err(|_| "memory byte count must be an integer".to_string())
        })
        .transpose()?
        .unwrap_or(128);
    if !(1..=1024 * 1024).contains(&count) {
        return Err("memory byte count must be between 1 and 1048576".to_string());
    }
    Ok((address.to_string(), count))
}

fn parse_breakpoint_options(input: &str) -> Result<BreakpointOptions, String> {
    let mut remaining = input.trim();
    let mut options = BreakpointOptions::default();
    loop {
        if let Some(rest) = remaining
            .strip_prefix("--temporary")
            .or_else(|| remaining.strip_prefix("-t"))
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            options.temporary = true;
            remaining = rest.trim_start();
        } else if let Some(rest) = remaining
            .strip_prefix("--hardware")
            .or_else(|| remaining.strip_prefix("-h"))
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            options.hardware = true;
            remaining = rest.trim_start();
        } else {
            break;
        }
    }
    remaining = remaining
        .strip_prefix("--condition")
        .or_else(|| remaining.strip_prefix("-c"))
        .or_else(|| remaining.strip_prefix("if"))
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .unwrap_or(remaining)
        .trim();
    if !remaining.is_empty() {
        options.condition = Some(remaining.to_string());
    }
    Ok(options)
}

fn apply_message(app: &mut App, message: UiMessage, requests: &mpsc::Sender<BackendRequest>) {
    match message {
        UiMessage::Capabilities(capabilities) => app.apply_capabilities(capabilities),
        UiMessage::Snapshot(snapshot) => {
            let session_membership_changed = app.sessions.len() != snapshot.sessions.len()
                || app.sessions.iter().any(|current| {
                    !snapshot
                        .sessions
                        .iter()
                        .any(|next| next.session_id == current.session_id)
                });
            app.apply_snapshot(snapshot);
            if session_membership_changed
                && app
                    .current_thread()
                    .is_some_and(|thread| thread.state.contains("stop"))
            {
                if let Some(thread_id) = app.current_thread().map(|thread| thread.id.clone()) {
                    inspect_thread(app, requests, thread_id);
                }
            }
        }
        UiMessage::Signals { target, signals } => {
            if let Err(error) = app.open_signal_picker(target, signals) {
                app.error(error);
            }
        }

        UiMessage::Threads(threads) => {
            let previous = app.current_thread().map(|thread| thread.id.clone());
            let thread_membership_changed = app.threads.len() != threads.len()
                || app
                    .threads
                    .iter()
                    .any(|current| !threads.iter().any(|next| next.thread_id == current.id));
            app.apply_threads(threads);
            let current = app.current_thread().map(|thread| thread.id.clone());
            let stopped = app
                .current_thread()
                .is_some_and(|thread| thread.state.contains("stop"));
            if stopped
                && (previous != current
                    || thread_membership_changed
                    || current.as_ref().is_some_and(|thread_id| {
                        app.frames.is_empty() && !app.thread_inspection_requested(thread_id)
                    }))
            {
                if let Some(thread_id) = current {
                    inspect_thread(app, requests, thread_id);
                }
            }
        }
        UiMessage::Frames {
            generation,
            thread_id,
            frames,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_frames(frames);
            }
        }
        UiMessage::DistributedFrames {
            generation,
            thread_id,
            result,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                let truncation = if result.truncated {
                    Some(
                        result
                            .truncation_reason
                            .unwrap_or_else(|| "the DDB runtime limit was reached".to_string()),
                    )
                } else {
                    None
                };
                app.apply_distributed_frames(result.frames);
                app.distributed_stack_truncation = truncation;
                if let Some(reason) = app.distributed_stack_truncation.clone() {
                    app.status = format!("DDB distributed stack is truncated: {reason}");
                    app.push_timeline(format!("⚠ {}", app.status));
                }
            }
        }
        UiMessage::Variables {
            generation,
            thread_id,
            variables,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_variables(variables);
            }
        }
        UiMessage::VariableChildren {
            generation,
            thread_id,
            parent_id,
            variables,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_variable_children(&parent_id, variables);
            }
        }
        UiMessage::Registers {
            generation,
            thread_id,
            registers,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_registers(registers);
            }
        }
        UiMessage::Memory {
            generation,
            thread_id,
            memory,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_memory(memory);
            }
        }
        UiMessage::Source {
            generation,
            thread_id,
            source,
            line,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_source(source, line);
            }
        }
        UiMessage::InspectionError {
            generation,
            thread_id,
            error,
        } => {
            if app.release_failed_inspection(generation, &thread_id) {
                app.error(error);
            }
        }
        UiMessage::Receipt(label, receipt) => {
            app.push_receipt(&label, &receipt);
            queue_request(app, requests, BackendRequest::Refresh);
        }
        UiMessage::Notice(message) => {
            app.status = message.clone();
            app.push_timeline(format!("◆ {message}"));
        }
        UiMessage::Output(output) => app.push_timeline(format!("│ {output}")),
        UiMessage::DebuggerEvent(event) => {
            app.push_timeline(format!("◆ {}", event.summary));
            match event.activity {
                DebuggerActivity::Running(thread_id) => app.mark_running(thread_id.as_deref()),
                DebuggerActivity::Stopped(Some(thread_id)) => {
                    let active_thread_id = app.current_thread().map(|thread| thread.id.clone());
                    let should_inspect = match active_thread_id.as_deref() {
                        None => true,
                        Some(active) if active == thread_id => {
                            !app.thread_inspection_requested(&thread_id)
                        }
                        Some(_) => false,
                    };
                    if should_inspect {
                        inspect_thread(app, requests, thread_id);
                    }
                }
                DebuggerActivity::Stopped(None) | DebuggerActivity::None => {}
            }
            if event.refresh {
                queue_request(app, requests, BackendRequest::Refresh);
            }
        }
        UiMessage::EventStream(status) => match status {
            EventStreamStatus::Connected => {
                app.event_stream_connected = true;
                app.push_timeline("◆ event stream connected");
            }
            EventStreamStatus::Reconnecting(error) => {
                app.event_stream_connected = false;
                app.push_timeline(format!("◆ event stream reconnecting: {error}"));
            }
        },
        UiMessage::Error(error) => app.error(error),
        UiMessage::BackendUnavailable(error) => app.backend_unavailable(error),
    }
}

fn queue_request(app: &mut App, requests: &mpsc::Sender<BackendRequest>, request: BackendRequest) {
    match requests.try_send(request) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(
            BackendRequest::Refresh | BackendRequest::Bootstrap,
        )) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            app.error("DDB request queue is full; wait for the current operation and retry")
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            app.backend_unavailable("DDB request worker is unavailable")
        }
    }
}

fn queue_recovery(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    let request = if app.api_connected {
        BackendRequest::Refresh
    } else {
        BackendRequest::Bootstrap
    };
    queue_request(app, requests, request);
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{DebuggerEvent, ThreadItem};

    #[tokio::test]
    async fn non_stop_debugger_events_do_not_restart_thread_inspection() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads.push(ThreadItem {
            id: "thread/17".to_string(),
            session_id: "session/1".to_string(),
            name: "main".to_string(),
            state: "stopped".to_string(),
            function: "work".to_string(),
            file: None,
            line: None,
        });
        let (requests, mut receiver) = mpsc::channel(4);

        apply_message(
            &mut app,
            UiMessage::DebuggerEvent(DebuggerEvent {
                summary: "BreakpointChanged".to_string(),
                refresh: false,
                activity: DebuggerActivity::None,
            }),
            &requests,
        );

        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn background_stop_does_not_steal_or_restart_the_active_thread() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads = vec![
            ThreadItem {
                id: "thread/child".to_string(),
                session_id: "session/child".to_string(),
                name: "child".to_string(),
                state: "stopped".to_string(),
                function: "leaf".to_string(),
                file: None,
                line: None,
            },
            ThreadItem {
                id: "thread/parent".to_string(),
                session_id: "session/parent".to_string(),
                name: "parent".to_string(),
                state: "stopped".to_string(),
                function: "handler".to_string(),
                file: None,
                line: None,
            },
        ];
        app.begin_inspection();
        let generation = app.inspection_generation;
        let (requests, mut receiver) = mpsc::channel(4);

        apply_message(
            &mut app,
            UiMessage::DebuggerEvent(DebuggerEvent {
                summary: "exec · stopped · thread/parent".to_string(),
                refresh: false,
                activity: DebuggerActivity::Stopped(Some("thread/parent".to_string())),
            }),
            &requests,
        );

        apply_message(
            &mut app,
            UiMessage::DebuggerEvent(DebuggerEvent {
                summary: "exec · stopped · thread/child".to_string(),
                refresh: false,
                activity: DebuggerActivity::Stopped(Some("thread/child".to_string())),
            }),
            &requests,
        );

        assert_eq!(app.current_thread().unwrap().id, "thread/child");
        assert_eq!(app.inspection_generation, generation);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn newly_ready_session_refreshes_the_selected_distributed_stack() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.sessions.push(v2::Session {
            session_id: "session/child".to_string(),
            display_name: "child".to_string(),
            revision: 1,
            ..Default::default()
        });
        app.threads.push(ThreadItem {
            id: "thread/child".to_string(),
            session_id: "session/child".to_string(),
            name: "main".to_string(),
            state: "stopped".to_string(),
            function: "leaf".to_string(),
            file: None,
            line: None,
        });
        let snapshot = v2::Snapshot {
            sessions: vec![
                app.sessions[0].clone(),
                v2::Session {
                    session_id: "session/parent".to_string(),
                    display_name: "parent".to_string(),
                    revision: 1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (requests, mut receiver) = mpsc::channel(1);

        apply_message(&mut app, UiMessage::Snapshot(snapshot), &requests);

        let Some(BackendRequest::InspectThread {
            thread_id,
            generation,
        }) = receiver.recv().await
        else {
            panic!("new DDB session membership should refresh the distributed stack");
        };
        assert_eq!(thread_id, "thread/child");
        assert_eq!(generation, app.inspection_generation);
    }

    #[tokio::test]
    async fn newly_ready_parent_thread_refreshes_the_selected_distributed_stack() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads.push(ThreadItem {
            id: "thread/child".to_string(),
            session_id: "session/child".to_string(),
            name: "child-main".to_string(),
            state: "stopped".to_string(),
            function: "leaf".to_string(),
            file: None,
            line: None,
        });
        let threads = vec![
            v2::Thread {
                thread_id: "thread/child".to_string(),
                session_id: "session/child".to_string(),
                name: Some("child-main".to_string()),
                state: v2::ThreadState::Stopped as i32,
                selected: true,
                revision: 1,
                ..Default::default()
            },
            v2::Thread {
                thread_id: "thread/parent".to_string(),
                session_id: "session/parent".to_string(),
                name: Some("parent-main".to_string()),
                state: v2::ThreadState::Stopped as i32,
                selected: false,
                revision: 1,
                ..Default::default()
            },
        ];
        let (requests, mut receiver) = mpsc::channel(1);

        apply_message(&mut app, UiMessage::Threads(threads), &requests);

        let Some(BackendRequest::InspectThread {
            thread_id,
            generation,
        }) = receiver.recv().await
        else {
            panic!("new DDB parent thread membership should refresh the distributed stack");
        };
        assert_eq!(thread_id, "thread/child");
        assert_eq!(generation, app.inspection_generation);
    }

    #[tokio::test]
    async fn stale_inspection_errors_do_not_replace_active_status() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads.push(ThreadItem {
            id: "thread/17".to_string(),
            session_id: "session/1".to_string(),
            name: "main".to_string(),
            state: "stopped".to_string(),
            function: "work".to_string(),
            file: None,
            line: None,
        });
        app.inspection_generation = 4;
        app.status = "active inspection".to_string();
        let (requests, _receiver) = mpsc::channel(1);

        apply_message(
            &mut app,
            UiMessage::InspectionError {
                generation: 3,
                thread_id: "thread/17".to_string(),
                error: "stale failure".to_string(),
            },
            &requests,
        );

        assert_eq!(app.status, "active inspection");
    }

    #[tokio::test]
    async fn failed_inspection_retries_on_next_stopped_projection() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads.push(ThreadItem {
            id: "thread/17".to_string(),
            session_id: "session/1".to_string(),
            name: "main".to_string(),
            state: "stopped".to_string(),
            function: "work".to_string(),
            file: None,
            line: None,
        });
        let generation = app.begin_inspection();
        let (requests, mut receiver) = mpsc::channel(1);

        apply_message(
            &mut app,
            UiMessage::InspectionError {
                generation,
                thread_id: "thread/17".to_string(),
                error: "transient failure".to_string(),
            },
            &requests,
        );

        apply_message(
            &mut app,
            UiMessage::Threads(vec![v2::Thread {
                thread_id: "thread/17".to_string(),
                session_id: "session/1".to_string(),
                state: v2::ThreadState::Stopped as i32,
                selected: true,
                ..Default::default()
            }]),
            &requests,
        );

        let request = receiver
            .try_recv()
            .expect("a stopped projection should retry the failed inspection");
        let BackendRequest::InspectThread {
            thread_id,
            generation: retried_generation,
        } = request
        else {
            panic!("a stopped projection should request thread inspection");
        };
        assert_eq!(thread_id, "thread/17");
        assert!(retried_generation > generation);
    }

    #[tokio::test]
    async fn active_truncated_distributed_stack_is_visible_and_stale_results_are_ignored() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads.push(ThreadItem {
            id: "thread/17".to_string(),
            session_id: "session/1".to_string(),
            name: "main".to_string(),
            state: "stopped".to_string(),
            function: "work".to_string(),
            file: None,
            line: None,
        });
        let generation = app.begin_inspection();
        let (requests, _receiver) = mpsc::channel(1);

        apply_message(
            &mut app,
            UiMessage::DistributedFrames {
                generation,
                thread_id: "thread/17".to_string(),
                result: v2::DistributedBacktraceResult {
                    frames: Vec::new(),
                    truncated: true,
                    truncation_reason: Some("max_frames limit reached".to_string()),
                },
            },
            &requests,
        );

        assert_eq!(
            app.distributed_stack_truncation.as_deref(),
            Some("max_frames limit reached")
        );
        assert_eq!(
            app.status,
            "DDB distributed stack is truncated: max_frames limit reached"
        );
        assert_eq!(
            app.timeline.back().map(String::as_str),
            Some("⚠ DDB distributed stack is truncated: max_frames limit reached")
        );

        let warning_count = app.timeline.len();
        app.begin_inspection();
        app.status = "newer inspection".to_string();
        apply_message(
            &mut app,
            UiMessage::DistributedFrames {
                generation,
                thread_id: "thread/17".to_string(),
                result: v2::DistributedBacktraceResult {
                    frames: Vec::new(),
                    truncated: true,
                    truncation_reason: Some("stale limit".to_string()),
                },
            },
            &requests,
        );

        assert!(app.distributed_stack_truncation.is_none());
        assert_eq!(app.status, "newer inspection");
        assert_eq!(app.timeline.len(), warning_count);
    }

    #[tokio::test]
    async fn disconnected_recovery_repeats_capability_discovery() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        let (requests, mut receiver) = mpsc::channel(1);

        queue_recovery(&mut app, &requests);
        assert!(matches!(
            receiver.recv().await,
            Some(BackendRequest::Bootstrap)
        ));

        app.api_connected = true;
        queue_recovery(&mut app, &requests);
        assert!(matches!(
            receiver.recv().await,
            Some(BackendRequest::Refresh)
        ));
    }

    #[tokio::test]
    async fn signal_shortcut_requests_typed_catalog_for_the_selected_ddb_target() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_capabilities(v2::Capabilities {
            api_version: "v2".to_string(),
            schema_version: "2.0.0-draft.3".to_string(),
            server_instance_id: "server".to_string(),
            execution_actions: vec![v2::ExecutionAction::Signal as i32],
            execution_action_capabilities: vec![v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Signal as i32,
                scopes: vec![v2::ExecutionScopeKind::Thread as i32],
            }],
            ..Default::default()
        });
        app.threads.push(ThreadItem {
            id: "thread/17".to_string(),
            session_id: "session/1".to_string(),
            name: "main".to_string(),
            state: "stopped".to_string(),
            function: "work".to_string(),
            file: None,
            line: None,
        });
        let (requests, mut receiver) = mpsc::channel(1);
        request_signal_picker(&mut app, &requests);
        let Some(BackendRequest::ListSignals { target }) = receiver.recv().await else {
            panic!("signal shortcut should request the typed DDB signal catalog");
        };
        assert!(matches!(
            target.selector,
            Some(v2::target::Selector::Thread(value)) if value.thread_id == "thread/17"
        ));
    }

    #[tokio::test]
    async fn group_step_is_rejected_from_advertised_scope_without_queueing() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_capabilities(v2::Capabilities {
            api_version: "v2".to_string(),
            schema_version: "2.0.0-draft.3".to_string(),
            server_instance_id: "server".to_string(),
            execution_actions: vec![v2::ExecutionAction::Next as i32],
            execution_action_capabilities: vec![v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Next as i32,
                scopes: vec![v2::ExecutionScopeKind::Thread as i32],
            }],
            ..Default::default()
        });
        app.execution_target = Some(api::group_target("group/a"));
        let (requests, mut receiver) = mpsc::channel(1);
        dispatch_control(&mut app, &requests, Control::Next);
        assert!(app.status.contains("not available for group"));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn extension_prompt_emits_declared_typed_action_request() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.extension_actions.push(model::ExtensionActionView {
            extension_id: "ddb.example".to_string(),
            extension_version: "1.2.0".to_string(),
            extension_title: "Example".to_string(),
            action_id: "rebalance".to_string(),
            title: "Rebalance".to_string(),
            description: "Move work".to_string(),
            request_schema_uri: "schema://rebalance".to_string(),
        });
        app.execution_target = Some(thread_target("thread/17"));
        app.start_extension_action_input().unwrap();
        app.input = r#"{"worker":"alpha"}"#.to_string();
        app.input_cursor = app.input.chars().count();
        let (requests, mut receiver) = mpsc::channel(1);
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &requests,
        );
        let Some(BackendRequest::InvokeExtensionAction {
            extension_id,
            extension_version,
            action_id,
            request_schema_uri,
            payload_json,
            ..
        }) = receiver.recv().await
        else {
            panic!("extension prompt should emit a typed extension action");
        };
        assert_eq!(extension_id, "ddb.example");
        assert_eq!(extension_version, "1.2.0");
        assert_eq!(action_id, "rebalance");
        assert_eq!(request_schema_uri, "schema://rebalance");
        assert_eq!(payload_json, r#"{"worker":"alpha"}"#);
    }

    #[test]
    fn parses_current_and_explicit_source_navigation() {
        assert_eq!(parse_source_navigation("37").unwrap(), (None, 37));
        assert_eq!(
            parse_source_navigation("/workspace/service.rs:37").unwrap(),
            (Some("/workspace/service.rs".to_string()), 37)
        );
        assert_eq!(
            parse_source_navigation(r"C:\workspace\service.rs:42").unwrap(),
            (Some(r"C:\workspace\service.rs".to_string()), 42)
        );
        assert!(parse_source_navigation("source.rs:zero").is_err());
        assert!(parse_source_navigation("0").is_err());
    }

    #[test]
    fn parses_memory_ranges_without_ambiguity_in_address_expressions() {
        assert_eq!(
            parse_memory_request("$sp + 16 ; 256").unwrap(),
            ("$sp + 16".to_string(), 256)
        );
        assert_eq!(
            parse_memory_request("request->buffer").unwrap(),
            ("request->buffer".to_string(), 128)
        );
        assert!(parse_memory_request("0x1000 ; 0").is_err());
        assert!(parse_memory_request("0x1000 ; many").is_err());
    }

    #[test]
    fn parses_advanced_breakpoint_options_and_condition() {
        assert_eq!(
            parse_breakpoint_options("-t if request.id == 42").unwrap(),
            BreakpointOptions {
                condition: Some("request.id == 42".to_string()),
                temporary: true,
                hardware: false,
            }
        );
        assert_eq!(
            parse_breakpoint_options("--hardware -c ptr == 0").unwrap(),
            BreakpointOptions {
                condition: Some("ptr == 0".to_string()),
                temporary: false,
                hardware: true,
            }
        );
        assert_eq!(
            parse_breakpoint_options("-t -h").unwrap(),
            BreakpointOptions {
                condition: None,
                temporary: true,
                hardware: true,
            }
        );
    }

    #[test]
    fn recognizes_tokenized_and_untokenized_raw_breakpoint_insert_commands() {
        assert!(is_raw_breakpoint_insert("23-break-insert /tmp/main.rs:7"));
        assert!(is_raw_breakpoint_insert("-break-insert /tmp/main.rs:7"));
        assert!(!is_raw_breakpoint_insert("-stack-list-frames"));
    }
}
