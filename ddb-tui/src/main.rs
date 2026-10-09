mod api;
mod cli;
mod display_text;
mod model;
mod output;
mod source_binding;
#[cfg(test)]
mod source_binding_tests;
mod source_browser;
mod source_explorer;
mod source_explorer_input;
#[cfg(test)]
mod source_explorer_interaction_tests;
mod source_provider;
mod source_types;
mod source_workspace;
mod supervisor;
mod ui;
mod worker;

use std::{io, time::Duration};

use anyhow::{Context, Result};
#[cfg(test)]
use api::BreakpointTarget;
use api::{thread_target, ApiClient, CapabilitiesExt, ClientConfig, TUI_API_COMPATIBILITY};
use clap::Parser;
use cli::{Args, Mode};
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
use source_browser::Navigation;
use source_explorer_input::{
    handle_key as handle_explorer_key, handle_mouse as handle_explorer_mouse,
};
use supervisor::ManagedBackend;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mode = args.parse_mode()?;
    let workspace = std::sync::Arc::new(match &args.source_workspace {
        Some(path) => source_workspace::Workspace::load(path)?,
        None => source_workspace::Workspace::default(),
    });

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
        args.refresh_ms,
        managed.as_ref(),
        workspace,
        args.values_formatting,
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
    refresh_ms: u64,
    managed: Option<&ManagedBackend>,
    workspace: std::sync::Arc<source_workspace::Workspace>,
    values_formatting: cli::ValuesFormatting,
) -> Result<()> {
    let client = negotiate_client(endpoint, token, managed).await?;
    let mut app = App::new(endpoint.to_string());
    app.source_workspace = workspace;
    app.values_formatting = values_formatting;
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
    managed: Option<&ManagedBackend>,
) -> Result<ApiClient> {
    let mut client_config = ClientConfig::new(endpoint);
    if let Some(token) = token {
        client_config = client_config.with_bearer_token(token.to_string());
    }
    let v2_client = ApiClient::new(client_config)?;
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
            Ok(v2_client)
        }
        Err(error) if managed.is_none() && error.is_retryable() => {
            Ok(v2_client)
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
    let mut worker_task = tokio::spawn(worker::run(
        client.clone(),
        app.source_workspace.clone(),
        request_rx,
        message_tx.clone(),
    ));
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
        Event::Paste(text) if app.source_explorer.visible => {
            source_explorer_input::edit_query(app, requests, &text)
        }
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
    if app.source_explorer.visible {
        handle_explorer_key(app, key, requests);
        return;
    }
    if app.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => app.show_help = false,
            KeyCode::Up | KeyCode::Char('k') => app.help_scroll = app.help_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                app.help_scroll = app.help_scroll.saturating_add(1).min(app.help_max_scroll)
            }
            KeyCode::PageUp => {
                app.help_scroll = app.help_scroll.saturating_sub(app.help_page_height)
            }
            KeyCode::PageDown => {
                app.help_scroll = app
                    .help_scroll
                    .saturating_add(app.help_page_height)
                    .min(app.help_max_scroll)
            }
            KeyCode::Home => app.help_scroll = 0,
            KeyCode::End => app.help_scroll = app.help_max_scroll,
            _ => {}
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
    if app.breakpoint_hit_picker.is_some() {
        match key.code {
            KeyCode::Esc => app.breakpoint_hit_picker = None,
            KeyCode::Up | KeyCode::Char('k') => app.move_breakpoint_hit(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_breakpoint_hit(1),
            KeyCode::Home => app.move_breakpoint_hit(isize::MIN),
            KeyCode::End => app.move_breakpoint_hit(isize::MAX),
            KeyCode::PageUp => {
                app.move_breakpoint_hit(-(app.areas.breakpoint_hit_rows.len().max(1) as isize))
            }
            KeyCode::PageDown => {
                app.move_breakpoint_hit(app.areas.breakpoint_hit_rows.len().max(1) as isize)
            }
            KeyCode::Enter => reveal_breakpoint_hit(app, requests),
            _ => {}
        }
        return;
    }
    if app.input_mode != InputMode::Normal {
        handle_prompt_key(app, key, requests);
        return;
    }

    match key.code {
        KeyCode::Char('z') => app.expanded_pane = !app.expanded_pane,
        KeyCode::Char('O') => {
            app.show_output = !app.show_output;
            if app.show_output {
                app.focus = Focus::Output;
            } else if app.focus == Focus::Output {
                app.focus = Focus::Source;
            }
        }
        KeyCode::Left | KeyCode::Right
            if app.focus == Focus::Output && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.output
                .scroll_horizontal(if key.code == KeyCode::Left { -8 } else { 8 });
        }
        KeyCode::Home if app.focus == Focus::Output => {
            app.output.scroll(isize::MIN);
            app.output.scroll_horizontal(isize::MIN);
        }
        KeyCode::End if app.focus == Focus::Output => app.output.follow(),
        KeyCode::Esc if app.expanded_pane => app.expanded_pane = false,
        KeyCode::Char('t') => {
            app.show_timeline = !app.show_timeline;
            if app.show_timeline {
                app.focus = Focus::Timeline;
            } else if app.focus == Focus::Timeline {
                app.focus = Focus::Source;
            }
        }
        KeyCode::Char('o') | KeyCode::Char('p')
            if key.code == KeyCode::Char('o') || key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            let request = app.source_explorer.open(key.code == KeyCode::Char('p'));
            queue_request(app, requests, BackendRequest::ExploreSources(request));
        }
        KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
            if let Some(request) = app.source_browser.history(key.code == KeyCode::Right) {
                queue_request(app, requests, BackendRequest::LoadSource(request));
            }
        }
        KeyCode::Left | KeyCode::Right if app.focus == Focus::Source => {
            app.source_browser
                .scroll_horizontal(if key.code == KeyCode::Left { -8 } else { 8 });
        }
        KeyCode::Home if app.focus == Focus::Source => {
            app.source_browser.scroll_horizontal(isize::MIN);
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Home
            if matches!(
                app.focus,
                Focus::Stack
                    | Focus::Variables
                    | Focus::Threads
                    | Focus::Breakpoints
                    | Focus::Registers
                    | Focus::Memory
            ) =>
        {
            let delta = match key.code {
                KeyCode::Left => -8,
                KeyCode::Right => 8,
                _ => isize::MIN,
            };
            match app.focus {
                Focus::Stack => app.stack_viewport.scroll(delta),
                Focus::Variables => app.variable_viewport.scroll(delta),
                Focus::Threads => app.topology_viewport.scroll(delta),
                Focus::Breakpoints => app.breakpoint_viewport.scroll(delta),
                Focus::Registers => app.register_viewport.scroll(delta),
                Focus::Memory => app.memory_viewport.scroll(delta),
                _ => unreachable!(),
            }
        }
        KeyCode::Char('f') if app.focus == Focus::Source => {
            app.source_browser.set_following(true);
            reveal_frame(app, requests, Navigation::Follow);
        }
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => {
            app.show_help = true;
            app.help_scroll = 0;
        }
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
        KeyCode::Char('=') if app.focus == Focus::Variables => start_variable_edit(app),
        KeyCode::Char('w') => app.start_input(InputMode::Watch),
        KeyCode::Delete | KeyCode::Backspace if app.focus == Focus::Variables => {
            if !app.remove_selected_watch() {
                app.status = "Select a watch expression to remove it".into();
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
        KeyCode::Char('P') if app.focus == Focus::Breakpoints => {
            if let Err(error) = app.open_breakpoint_hits() {
                app.error(error);
            }
        }
        KeyCode::Char('F') => app.start_input(InputMode::FunctionBreakpoint),
        KeyCode::Char('J') => start_execution_prompt(app, "jump", InputMode::Jump),
        KeyCode::Char('s') => request_signal_picker(app, requests),
        KeyCode::Char('g') => app.start_input(InputMode::GotoLine),
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
        KeyCode::Up | KeyCode::Char('k') => {
            move_selection(app, requests, -1);
            inspect_selection(app, requests);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            move_selection(app, requests, 1);
            inspect_selection(app, requests);
        }
        KeyCode::PageUp => {
            move_selection(app, requests, -page_size(app));
            inspect_selection(app, requests);
        }
        KeyCode::PageDown => {
            move_selection(app, requests, page_size(app));
            inspect_selection(app, requests);
        }
        KeyCode::Enter if app.focus == Focus::Extensions => {
            if let Err(error) = app.start_extension_action_input() {
                app.error(error);
            }
        }
        KeyCode::Enter if app.focus == Focus::Breakpoints => reveal_breakpoint(app, requests),
        KeyCode::Enter | KeyCode::Right if app.focus == Focus::Variables => {
            toggle_selected_variable(app, requests)
        }
        KeyCode::Enter => inspect_selection(app, requests),
        KeyCode::F(5) => dispatch_control(app, requests, Control::Continue),
        KeyCode::F(6) => dispatch_control(app, requests, Control::Interrupt),
        KeyCode::F(8) => reveal_selected_frame(app, requests),
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
        KeyCode::Up | KeyCode::Char('k') => app.move_breakpoint_target_picker(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_breakpoint_target_picker(1),
        KeyCode::PageUp => app.move_breakpoint_target_picker(
            -(app.areas.breakpoint_target_rows.len().max(1) as isize),
        ),
        KeyCode::PageDown => app
            .move_breakpoint_target_picker(app.areas.breakpoint_target_rows.len().max(1) as isize),
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
                location: draft.location,
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
                    InputMode::AssignVariable => {
                        if let Some(edit) = app.variable_edit.take() {
                            if app.inspection_is_current(edit.generation, &edit.origin_thread)
                                && app.current_frame().is_some_and(|f| f.id == edit.frame_id)
                            {
                                queue_request(
                                    app,
                                    requests,
                                    BackendRequest::AssignVariable { edit, value: input },
                                );
                            } else {
                                app.error("The variable's inspection context changed. Select it again before editing.");
                            }
                        }
                    }
                    InputMode::Command => {
                        if app.current_frame().is_some_and(|frame| frame.boundary) {
                            app.status =
                                "select an inspectable frame before sending a native command"
                                    .into();
                            return;
                        }
                        let context = app
                            .current_frame()
                            .map(|frame| {
                                (
                                    thread_target(frame.thread_id.clone()),
                                    Some(frame.id.clone()),
                                )
                            })
                            .or_else(|| {
                                app.current_thread()
                                    .map(|thread| (thread_target(thread.id.clone()), None))
                            });
                        if let Some((target, frame_id)) = context {
                            app.show_output = true;
                            app.focus = Focus::Output;
                            queue_request(
                                app,
                                requests,
                                BackendRequest::RawCommand {
                                    command: input,
                                    target,
                                    frame_id,
                                },
                            );
                        } else {
                            app.status = "select a thread before sending a native debugger command"
                                .to_string();
                        }
                    }
                    InputMode::Evaluate => {
                        if let Some(frame) =
                            app.current_frame().filter(|frame| !frame.boundary).cloned()
                        {
                            let thread_id = frame.thread_id;
                            let frame_id = Some(frame.id);
                            app.show_output = true;
                            app.focus = Focus::Output;
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
                            app.status =
                                "select an inspectable frame before evaluating".to_string();
                        }
                    }
                    InputMode::Watch => match app.add_watch(input) {
                        Ok(()) => refresh_watches(app, requests),
                        Err(error) => app.error(error),
                    },
                    InputMode::Memory => {
                        if app.current_frame().is_some_and(|frame| frame.boundary) {
                            app.error("Select an inspectable frame before reading memory");
                            return;
                        }
                        if let Some(thread_id) =
                            app.current_thread().map(|thread| thread.id.clone())
                        {
                            let owner_thread = app
                                .current_frame()
                                .map_or_else(|| thread_id.clone(), |frame| frame.thread_id.clone());
                            match parse_memory_request(&input) {
                                Ok((address, count)) => {
                                    app.focus = Focus::Memory;
                                    queue_request(
                                        app,
                                        requests,
                                        BackendRequest::ReadMemory {
                                            address,
                                            count,
                                            thread_id,
                                            owner_thread,
                                            generation: app.inspection_generation,
                                        },
                                    );
                                }
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
                    InputMode::FunctionBreakpoint => {
                        let result =
                            parse_function_breakpoint(&input).and_then(|(name, options)| {
                                app.start_function_breakpoint_picker(name, options)
                            });
                        if let Err(error) = result {
                            app.error(error);
                        }
                    }
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
    app.status = format!("loading signals for {}…", app.execution_scope_label());
    queue_request(app, requests, BackendRequest::ListSignals { target });
}

fn handle_signal_picker_key(app: &mut App, key: KeyEvent, requests: &mpsc::Sender<BackendRequest>) {
    match key.code {
        KeyCode::Esc => app.cancel_signal_picker(),
        KeyCode::Up | KeyCode::Char('k') => app.move_signal_picker(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_signal_picker(1),
        KeyCode::PageUp => app.move_signal_picker(-(app.areas.signal_rows.len().max(1) as isize)),
        KeyCode::PageDown => app.move_signal_picker(app.areas.signal_rows.len().max(1) as isize),
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
    if app.show_help {
        match mouse.kind {
            MouseEventKind::ScrollUp => app.help_scroll = app.help_scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => {
                app.help_scroll = app.help_scroll.saturating_add(3).min(app.help_max_scroll)
            }
            _ => {}
        }
        return;
    }
    if app.source_explorer.visible {
        handle_explorer_mouse(app, mouse, requests);
        return;
    }
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

    if app.breakpoint_hit_picker.is_some() {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let position = ratatui::layout::Position::new(mouse.column, mouse.row);
                if app
                    .areas
                    .breakpoint_hit_cancel
                    .is_some_and(|area| area.contains(position))
                {
                    app.breakpoint_hit_picker = None;
                } else if let Some((index, _)) = app
                    .areas
                    .breakpoint_hit_rows
                    .iter()
                    .find(|(_, area)| area.contains(position))
                {
                    app.breakpoint_hit_picker.as_mut().unwrap().cursor = *index;
                    reveal_breakpoint_hit(app, requests);
                }
            }
            MouseEventKind::ScrollUp => app.move_breakpoint_hit(-3),
            MouseEventKind::ScrollDown => app.move_breakpoint_hit(3),
            _ => {}
        }
        return;
    }

    match mouse.kind {
        MouseEventKind::Down(button) => {
            if button == MouseButton::Left {
                if let Some((focus, _)) = app.areas.pane_expanders.iter().find(|(_, area)| {
                    area.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                }) {
                    app.expanded_pane = !app.expanded_pane || app.focus != *focus;
                    app.focus = *focus;
                    return;
                }
            }
            if button == MouseButton::Left
                && app.areas.source_explorer_open_at(mouse.column, mouse.row)
            {
                let request = app.source_explorer.open(false);
                queue_request(app, requests, BackendRequest::ExploreSources(request));
                return;
            }
            if let Some(control) = app.areas.control_at(mouse.column, mouse.row) {
                match control {
                    Control::Refresh => queue_recovery(app, requests),
                    Control::CycleScope => app.cycle_execution_scope(),
                    Control::RefreshStack => refresh_stack(app, requests),
                    Control::RevealFrame => reveal_selected_frame(app, requests),
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
                app.selected_breakpoint_site = app
                    .areas
                    .breakpoint_sites
                    .iter()
                    .find(|(_, _, area)| {
                        area.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                    })
                    .and_then(|(_, site, _)| *site);
                if button == MouseButton::Left {
                    reveal_breakpoint(app, requests);
                } else if button == MouseButton::Right {
                    if let Err(error) = app.open_breakpoint_hits() {
                        app.error(error);
                    }
                }
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
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
            if let Some(focus) = app.areas.panel_at(mouse.column, mouse.row) {
                let delta = if mouse.kind == MouseEventKind::ScrollLeft {
                    -8
                } else {
                    8
                };
                match focus {
                    Focus::Source => app.source_browser.scroll_horizontal(delta),
                    Focus::Stack => app.stack_viewport.scroll(delta),
                    Focus::Variables => app.variable_viewport.scroll(delta),
                    Focus::Threads => app.topology_viewport.scroll(delta),
                    Focus::Breakpoints => app.breakpoint_viewport.scroll(delta),
                    Focus::Registers => app.register_viewport.scroll(delta),
                    Focus::Memory => app.memory_viewport.scroll(delta),
                    Focus::Output => app.output.scroll_horizontal(delta),
                    _ => return,
                }
                app.focus = focus;
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(focus) = app.areas.panel_at(mouse.column, mouse.row) {
                app.focus = focus;
            }
            move_selection(app, requests, -3);
            inspect_selection(app, requests);
        }
        MouseEventKind::ScrollDown => {
            if let Some(focus) = app.areas.panel_at(mouse.column, mouse.row) {
                app.focus = focus;
            }
            move_selection(app, requests, 3);
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
            if let Some(source) = app.source_browser.document() {
                let line = (source.start_line + item).min(source.end_line);
                app.source_browser.set_cursor(line);
            }
            true
        }
        Focus::Timeline => {
            app.timeline_scroll = item.min(app.timeline.len().saturating_sub(1));
            true
        }
        Focus::Output => false,
        Focus::Extensions => {
            app.extension_scroll = item;
            true
        }
        Focus::Variables => {
            app.selected_variable = item.min(app.variables.len().saturating_sub(1));
            true
        }
        Focus::Registers => {
            app.selected_register = item.min(app.registers.len().saturating_sub(1));
            true
        }
        Focus::Memory => {
            app.selected_memory_row = item.min(app.memory_row_count().saturating_sub(1));
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
        let request = if app.variables[app.selected_variable].scope {
            BackendRequest::ExpandScope {
                scope_id: variable_id,
                thread_id,
                generation,
            }
        } else {
            BackendRequest::ExpandVariable {
                variable_id,
                thread_id,
                generation,
            }
        };
        if let Err(error) = requests.try_send(request) {
            app.variables[app.selected_variable].loading = false;
            match error {
                mpsc::error::TrySendError::Full(_) => {
                    app.error("DDB request queue is full; retry expansion")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    app.backend_unavailable("DDB request worker is unavailable")
                }
            }
        }
    }
}

fn refresh_watches(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    for request in app.prepare_watches() {
        if let Err(error) = requests.try_send(BackendRequest::EvaluateWatch(request.clone())) {
            app.finish_watch(request, Err(format!("Could not queue evaluation: {error}")));
        }
    }
}

fn start_variable_edit(app: &mut App) {
    let Some(variable) = app
        .variables
        .get(app.selected_variable)
        .filter(|v| !v.scope && !v.id.is_empty())
    else {
        app.status = "Select a variable value, not a scope, to edit.".into();
        return;
    };
    let Some(frame) = app.current_frame().filter(|f| !f.boundary) else {
        app.status = "Select a stopped, inspectable frame to edit variables.".into();
        return;
    };
    let Some(origin) = app.current_thread() else {
        return;
    };
    let edit = model::VariableEdit {
        variable_id: variable.id.clone(),
        owner_thread: frame.thread_id.clone(),
        origin_thread: origin.id.clone(),
        frame_id: frame.id.clone(),
        generation: app.inspection_generation,
    };
    let name = variable.name.clone();
    app.start_input(InputMode::AssignVariable);
    app.variable_edit = Some(edit);
    app.status = format!("Assign {name}: enter a value in the debugger's assignment syntax");
}

fn page_size(app: &App) -> isize {
    if app.focus == Focus::Breakpoints && !app.areas.breakpoint_sites.is_empty() {
        return app.areas.breakpoint_sites.len() as isize;
    }
    app.areas
        .panels
        .iter()
        .find(|panel| panel.focus == app.focus)
        .map(|panel| {
            (panel.area.height.saturating_sub(2) as usize / panel.item_height.max(1)).max(1)
                as isize
        })
        .unwrap_or(10)
}

fn move_selection(app: &mut App, requests: &mpsc::Sender<BackendRequest>, delta: isize) {
    if app.focus == Focus::Source {
        if let Some(request) = app.source_browser.move_cursor(delta) {
            queue_request(app, requests, BackendRequest::LoadSource(request));
        }
    } else {
        app.move_selection(delta);
    }
}

fn inspect_selection(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    match app.focus {
        Focus::Threads => {
            if let Some(thread_id) = app.selected_topology_thread_id().map(str::to_string) {
                if app
                    .threads
                    .iter()
                    .any(|t| t.id == thread_id && t.state == "stopped")
                {
                    app.source_browser.set_following(true);
                }
                inspect_thread(app, requests, thread_id);
            }
        }
        Focus::Stack => {
            if let (Some(thread_id), Some(frame)) = (
                app.current_thread().map(|thread| thread.id.clone()),
                app.current_frame().cloned(),
            ) {
                let generation = app.begin_frame_inspection();
                if frame.boundary {
                    app.status = format!(
                        "Distributed boundary {} has no inspectable frame. Select an adjacent frame.",
                        frame.function
                    );
                    app.source_browser.unavailable(app.status.clone());
                    return;
                }
                reveal_frame(app, requests, Navigation::Frame);
                queue_request(
                    app,
                    requests,
                    BackendRequest::InspectFrame {
                        thread_id,
                        generation,
                        frame_id: frame.id,
                    },
                );
                refresh_watches(app, requests);
            }
        }
        _ => {}
    }
}

fn reveal_breakpoint_hit(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    match app.commit_breakpoint_hit() {
        Ok(thread_id) => {
            app.focus = Focus::Source;
            inspect_thread(app, requests, thread_id);
        }
        Err(error) => app.error(error),
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
    let indices = app.source_breakpoint_indices();
    if indices.len() > 1 {
        app.focus = Focus::Breakpoints;
        app.selected_breakpoint = indices[0];
        app.status =
            "multiple DDB breakpoints at this location; select one in Breakpoints and press Delete"
                .into();
        return;
    }
    if let Some(index) = indices.first() {
        let breakpoint = &app.breakpoints[*index];
        let Some(api_target) = breakpoint.target.clone() else {
            app.error("breakpoint has no explicit target; refresh before deleting it");
            return;
        };
        let id = breakpoint.id.clone();
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
    } else if !["conditional", "temporary", "hardware", "ignore_count"]
        .into_iter()
        .any(|feature| app.capabilities.supports_breakpoint_action(feature))
    {
        app.status = "advanced breakpoints are not supported by this DDB API".to_string();
    } else if let Err(error) = app.source_breakpoint_location() {
        app.error(error);
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
    let Ok(api_line) = u32::try_from(line.max(1)) else {
        return app.error("source line exceeds the API limit");
    };
    let request = match requested_source {
        Some(path) => app.source_browser.navigate(
            None,
            v2::SourceLocation {
                path: Some(path),
                line: api_line,
                ..Default::default()
            },
            Navigation::Manual,
        ),
        None => app.source_browser.go_to_line(line),
    };
    if let Some(request) = request {
        app.focus = Focus::Source;
        queue_request(app, requests, BackendRequest::LoadSource(request));
    } else {
        app.status = "enter path:line when no source file is loaded".to_string();
    }
}

fn reveal_selected_frame(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    match app.current_frame() {
        None => app.status = "No stack frame selected. Select a stopped thread first.".into(),
        Some(frame) if frame.boundary => {
            app.status =
                "This distributed boundary is not a source frame. Select an adjacent frame.".into();
            app.source_browser.unavailable(app.status.clone());
            app.focus = Focus::Source;
        }
        Some(frame) if frame.file.is_none() || frame.line.is_none() => {
            app.status = format!(
                "Source unavailable for {}: debugger supplied no file/line location.",
                frame.function
            );
            app.source_browser.unavailable(app.status.clone());
            app.focus = Focus::Source;
        }
        Some(_) => {
            app.focus = Focus::Source;
            reveal_frame(app, requests, Navigation::Frame);
        }
    }
}

fn reveal_breakpoint(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    let Some(bp) = app.current_breakpoint() else {
        return;
    };
    let (target, location) = if let Some(site) = app.selected_breakpoint_site {
        let Some(member) = bp.sub_breakpoints.get(site) else {
            return;
        };
        let Some(location) = member
            .location
            .clone()
            .filter(|l| l.line > 0 && (l.path.is_some() || l.source_reference.is_some()))
        else {
            app.status = "This breakpoint site has no resolved source location yet.".into();
            return;
        };
        (
            Some(crate::api::session_target(&member.session_id)),
            location,
        )
    } else {
        if bp.sub_breakpoints.len() > 1 {
            app.status = "Select a session-specific breakpoint site with Down, then Enter to open its source.".into();
            return;
        }
        if let Some(member) = bp.sub_breakpoints.first().filter(|s| {
            s.location
                .as_ref()
                .is_some_and(|l| l.line > 0 && (l.path.is_some() || l.source_reference.is_some()))
        }) {
            (
                Some(crate::api::session_target(&member.session_id)),
                member.location.clone().unwrap(),
            )
        } else if let Some(line) = u32::try_from(bp.location.line)
            .ok()
            .filter(|line| *line > 0)
        {
            (
                bp.target.clone(),
                v2::SourceLocation {
                    path: Some(bp.location.src.clone()),
                    line,
                    ..Default::default()
                },
            )
        } else {
            app.status =
                "This breakpoint has no source location yet. Select a resolved session site."
                    .into();
            return;
        }
    };
    if let Some(request) = app
        .source_browser
        .navigate(target, location, Navigation::Manual)
    {
        app.focus = Focus::Source;
        queue_request(app, requests, BackendRequest::LoadSource(request));
    }
}

fn reveal_frame(app: &mut App, requests: &mpsc::Sender<BackendRequest>, navigation: Navigation) {
    if navigation == Navigation::Follow && !app.source_browser.following() {
        return;
    }
    let frame = if navigation == Navigation::Follow {
        app.frames
            .iter()
            .filter(|frame| {
                !frame.boundary
                    && app
                        .current_thread()
                        .is_some_and(|thread| frame.thread_id == thread.id)
            })
            .min_by_key(|frame| frame.distributed_index)
    } else {
        app.current_frame().filter(|frame| !frame.boundary)
    };
    let Some(frame) = frame.cloned() else { return };
    let (Some(path), Some(line)) = (frame.file, frame.line) else {
        app.source_browser.unavailable(format!(
            "Source unavailable for {}: debugger supplied no file/line location.",
            frame.function
        ));
        return;
    };
    let Ok(line) = u32::try_from(line) else {
        return;
    };
    let target = Some(thread_target(frame.thread_id));
    let location = v2::SourceLocation {
        path: Some(path),
        line,
        ..Default::default()
    };
    if let Some(request) = app.source_browser.navigate(target, location, navigation) {
        queue_request(app, requests, BackendRequest::LoadSource(request));
    }
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

fn parse_function_breakpoint(input: &str) -> Result<(String, BreakpointOptions), String> {
    let (name, options) = input.split_once(" -- ").unwrap_or((input, ""));
    if name.trim().is_empty() {
        return Err("Enter a function or symbol name".into());
    }
    Ok((name.trim().into(), parse_breakpoint_options(options)?))
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
        } else if let Some(rest) = remaining
            .strip_prefix("--ignore-count")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            if options.ignore_count.is_some() {
                return Err("Specify --ignore-count only once".into());
            }
            let rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let count = &rest[..end];
            if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("--ignore-count requires a nonnegative integer".into());
            }
            options.ignore_count = Some(
                count
                    .parse()
                    .map_err(|_| "Ignore count exceeds the unsigned 64-bit range")?,
            );
            remaining = rest[end..].trim_start();
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
        UiMessage::WatchEvaluated { request, result } => app.finish_watch(request, result),
        UiMessage::VariableAssigned { edit, result } => {
            if !app.inspection_is_current(edit.generation, &edit.origin_thread)
                || !app.current_frame().is_some_and(|f| f.id == edit.frame_id)
            {
                return;
            }
            match result {
                Ok(receipt) => {
                    let completed = receipt.state == v2::OperationState::Completed as i32;
                    app.push_receipt("assign variable", &receipt);
                    if completed {
                        let generation = app.begin_frame_inspection();
                        queue_request(
                            app,
                            requests,
                            BackendRequest::InspectFrame {
                                thread_id: edit.origin_thread,
                                generation,
                                frame_id: edit.frame_id,
                            },
                        );
                        refresh_watches(app, requests);
                    }
                }
                Err(error) => app.error(error),
            }
        }
        UiMessage::PeerPauseFailed {
            session_id,
            epoch,
            error,
        } => {
            if app.control_epoch.load(std::sync::atomic::Ordering::SeqCst) == epoch {
                app.peer_pauses.remove(&session_id);
                app.error(format!("Could not pause DDB session {session_id}: {error}"));
            }
        }
        UiMessage::Capabilities(capabilities) => {
            app.apply_capabilities(capabilities);
            configure_value_presentation(app, requests);
        }
        UiMessage::Snapshot(snapshot) => {
            let new_stop = app.has_new_inspection_stop(&snapshot);
            let session_membership_changed = app.sessions.len() != snapshot.sessions.len()
                || app.sessions.iter().any(|current| {
                    !snapshot
                        .sessions
                        .iter()
                        .any(|next| next.session_id == current.session_id)
                });
            app.apply_snapshot(snapshot);
            configure_value_presentation(app, requests);
            if (session_membership_changed || new_stop)
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
                refresh_watches(app, requests);
                reveal_frame(app, requests, Navigation::Follow);
                app.distributed_stack_truncation = truncation;
                if let Some(reason) = app.distributed_stack_truncation.clone() {
                    app.status = format!("DDB distributed stack is truncated: {reason}");
                    app.push_timeline(format!("⚠ {}", app.status));
                }
            }
        }
        UiMessage::Scopes {
            generation,
            thread_id,
            scopes,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                app.apply_scopes(scopes);
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
        UiMessage::SourceDiscovery { generation, result } => match result {
            Ok(page) => {
                app.source_explorer.apply(generation, page);
            }
            Err(error) => {
                app.source_explorer.fail(generation, error);
            }
        },
        UiMessage::Source { generation, source } => {
            app.source_browser.apply(generation, source);
        }
        UiMessage::SourceError { generation, error } => {
            if app
                .source_browser
                .fail_with_error(generation, error.clone())
            {
                app.error(error);
            }
        }
        UiMessage::ExpansionError {
            generation,
            thread_id,
            parent_id,
            error,
        } => {
            if app.inspection_is_current(generation, &thread_id) {
                if let Some(variable) = app
                    .variables
                    .iter_mut()
                    .find(|variable| variable.id == parent_id)
                {
                    variable.loading = false;
                }
                app.error(error);
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
        UiMessage::Output(output) => app.output.push(&output),
        UiMessage::DebuggerEvent(event) => {
            app.push_timeline(format!("◆ {}", event.summary));
            match event.activity {
                DebuggerActivity::Running(thread_id) => {
                    if let Some(thread) = app
                        .threads
                        .iter()
                        .find(|t| Some(&t.id) == thread_id.as_ref())
                    {
                        app.peer_pauses.remove(&thread.session_id);
                    }
                    app.mark_running(thread_id.as_deref());
                    if let Some(cause) = app.all_stop_cause.clone() {
                        if app
                            .threads
                            .iter()
                            .any(|t| t.id == cause && t.state == "stopped")
                        {
                            pause_running_peers(app, requests, &cause);
                        } else {
                            app.all_stop_cause = None;
                        }
                    }
                }
                DebuggerActivity::Stopped(Some(thread_id)) => {
                    app.all_stop_cause.get_or_insert_with(|| thread_id.clone());
                    pause_running_peers(app, requests, &thread_id);
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

fn pause_running_peers(
    app: &mut App,
    requests: &mpsc::Sender<BackendRequest>,
    stopped_thread: &str,
) {
    let Some(owner) = app
        .threads
        .iter()
        .find(|t| t.id == stopped_thread)
        .map(|t| t.session_id.clone())
    else {
        return;
    };
    // Update the cause before considering peer stops so a later peer event
    // cannot send an interrupt back to the original stopped process.
    for thread in &mut app.threads {
        if thread.id == stopped_thread {
            thread.state = "stopped".into();
        }
    }
    app.peer_pauses.insert(owner.clone());
    let peers = app
        .threads
        .iter()
        .filter(|t| t.state == "running" && t.session_id != owner)
        .map(|t| t.session_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for session_id in peers {
        if app.peer_pauses.contains(&session_id) {
            continue;
        }
        let target = crate::api::session_target(&session_id);
        if !app
            .capabilities
            .supports_execution_target("interrupt", &target)
        {
            app.error(format!(
                "Cannot enforce all-stop for {session_id}: session interrupt is unsupported"
            ));
            continue;
        }
        let epoch = app.control_epoch.load(std::sync::atomic::Ordering::SeqCst);
        let request = BackendRequest::PausePeer {
            session_id: session_id.clone(),
            epoch,
            current_epoch: app.control_epoch.clone(),
        };
        match requests.try_send(request) {
            Ok(()) => {
                app.peer_pauses.insert(session_id);
            }
            Err(error) => app.error(format!(
                "Could not queue all-stop pause for {session_id}: {error}"
            )),
        }
    }
}

fn configure_value_presentation(app: &mut App, requests: &mpsc::Sender<BackendRequest>) {
    use cli::ValuesFormatting;
    if app.values_formatting != ValuesFormatting::PrettyPrinters
        || !app
            .capabilities
            .supported_operations
            .contains(&(v2::OperationKind::ConfigureDebugger as i32))
    {
        return;
    }
    let sessions = app
        .sessions
        .iter()
        .filter(|session| {
            session
                .backend
                .as_ref()
                .is_some_and(|backend| backend.kind == v2::BackendKind::Gdb as i32)
                && matches!(
                    v2::SessionStatus::try_from(session.status),
                    Ok(v2::SessionStatus::Ready | v2::SessionStatus::Stopped)
                )
                && !app.pretty_printing_attempted.contains(&session.session_id)
        })
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    for session_id in sessions {
        match requests.try_send(BackendRequest::EnablePrettyPrinting {
            session_id: session_id.clone(),
        }) {
            Ok(()) => {
                app.pretty_printing_attempted.insert(session_id);
            }
            Err(error) => app.error(format!(
                "Could not queue pretty-printer setup for {session_id}: {error}"
            )),
        }
    }
}

fn queue_request(app: &mut App, requests: &mpsc::Sender<BackendRequest>, request: BackendRequest) {
    // Explicit execution intent supersedes automatic work even if the queue is
    // full. The user must never be resumed/paused by an older automatic task
    // while retrying their command. Inspection and automatic peer pauses do not
    // advance this epoch.
    if matches!(
        &request,
        BackendRequest::Control(..)
            | BackendRequest::Jump { .. }
            | BackendRequest::SendSignal { .. }
            | BackendRequest::RawCommand { .. }
    ) {
        app.supersede_automatic_control();
    }
    match requests.try_send(request) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(
            BackendRequest::Refresh | BackendRequest::Bootstrap,
        )) => {}
        Err(mpsc::error::TrySendError::Full(request)) => {
            if let BackendRequest::ExploreSources(request) = &request {
                app.source_explorer.fail(
                    request.generation,
                    "DDB request queue is full; retry the source request".into(),
                );
            } else if let BackendRequest::LoadSource(request) = request {
                app.source_browser.fail(request.generation);
            }
            app.error("DDB request queue is full; wait for the current operation and retry");
        }
        Err(mpsc::error::TrySendError::Closed(request)) => {
            if let BackendRequest::ExploreSources(request) = &request {
                app.source_explorer.fail(
                    request.generation,
                    "DDB request worker is unavailable".into(),
                );
            } else if let BackendRequest::LoadSource(request) = request {
                app.source_browser.fail(request.generation);
            }
            app.backend_unavailable("DDB request worker is unavailable");
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

    #[test]
    fn memory_navigation_is_local_and_read_uses_distributed_frame_owner() {
        let mut app = distributed_caller_app();
        let (tx, mut rx) = mpsc::channel(4);
        app.start_input(InputMode::Memory);
        app.input = "0x1000 ; 256".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::ReadMemory { thread_id, owner_thread, count: 256, .. } if thread_id == "thread/child" && owner_thread == "thread/parent")
        );
        assert_eq!(app.focus, Focus::Memory);
        app.apply_memory(v2::MemoryBlock {
            address: "0x1000".into(),
            data: vec![0; 256],
            ..Default::default()
        });
        app.areas.add_panel(
            Focus::Memory,
            ratatui::layout::Rect::new(10, 5, 30, 8),
            0,
            1,
        );
        app.memory_viewport.fit(85, 26);
        for code in [KeyCode::PageDown, KeyCode::Right] {
            handle_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE), &tx);
        }
        assert_eq!(app.selected_memory_row, 6);
        assert_eq!(app.memory_viewport.column(), 8);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollRight,
                column: 15,
                row: 8,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert_eq!(app.memory_viewport.column(), 16);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.memory_viewport.column(), 0);
        assert!(rx.try_recv().is_err());
        app.frames[0].boundary = true;
        app.start_input(InputMode::Memory);
        app.input = "0x1000".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(app.status.contains("inspectable frame"));
    }

    #[test]
    fn pretty_printer_setup_is_capability_gated_and_once_per_ready_gdb_session() {
        let mut app = App::new("http://localhost".into());
        let session = |id: &str, kind: v2::BackendKind, status: v2::SessionStatus| v2::Session {
            session_id: id.into(),
            backend: Some(v2::BackendDescriptor {
                kind: kind as i32,
                ..Default::default()
            }),
            status: status as i32,
            ..Default::default()
        };
        app.sessions = vec![
            session("gdb", v2::BackendKind::Gdb, v2::SessionStatus::Stopped),
            session("lldb", v2::BackendKind::Lldb, v2::SessionStatus::Stopped),
            session(
                "starting",
                v2::BackendKind::Gdb,
                v2::SessionStatus::Starting,
            ),
        ];
        let (tx, mut rx) = mpsc::channel(8);
        configure_value_presentation(&mut app, &tx);
        assert!(rx.try_recv().is_err());
        app.capabilities
            .supported_operations
            .push(v2::OperationKind::ConfigureDebugger as i32);
        configure_value_presentation(&mut app, &tx);
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::EnablePrettyPrinting { session_id } if session_id == "gdb")
        );
        configure_value_presentation(&mut app, &tx);
        assert!(rx.try_recv().is_err());
        app.sessions[2].status = v2::SessionStatus::Stopped as i32;
        configure_value_presentation(&mut app, &tx);
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::EnablePrettyPrinting { session_id } if session_id == "starting")
        );
        assert!(rx.try_recv().is_err());
        for mode in [cli::ValuesFormatting::Raw, cli::ValuesFormatting::Disabled] {
            app.pretty_printing_attempted.clear();
            app.values_formatting = mode;
            configure_value_presentation(&mut app, &tx);
            assert!(rx.try_recv().is_err());
        }
        app.values_formatting = cli::ValuesFormatting::PrettyPrinters;
        let (full_tx, mut full_rx) = mpsc::channel(1);
        full_tx.try_send(BackendRequest::Refresh).unwrap();
        configure_value_presentation(&mut app, &full_tx);
        assert!(app.pretty_printing_attempted.is_empty());
        full_rx.try_recv().unwrap();
        configure_value_presentation(&mut app, &full_tx);
        assert!(matches!(
            full_rx.try_recv().unwrap(),
            BackendRequest::EnablePrettyPrinting { .. }
        ));
    }

    #[test]
    fn pretty_printer_setup_survives_refresh_but_resets_for_a_new_backend_instance() {
        let mut app = App::new("http://localhost".into());
        app.capabilities
            .supported_operations
            .push(v2::OperationKind::ConfigureDebugger as i32);
        let snapshot = |instance: &str| v2::Snapshot {
            server_instance_id: instance.into(),
            sessions: vec![v2::Session {
                session_id: "session/gdb".into(),
                status: v2::SessionStatus::Stopped as i32,
                backend: Some(v2::BackendDescriptor {
                    kind: v2::BackendKind::Gdb as i32,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (tx, mut rx) = mpsc::channel(4);
        app.apply_snapshot(snapshot("instance/one"));
        configure_value_presentation(&mut app, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            BackendRequest::EnablePrettyPrinting { .. }
        ));
        app.apply_snapshot(snapshot("instance/one"));
        configure_value_presentation(&mut app, &tx);
        assert!(rx.try_recv().is_err());
        app.apply_snapshot(snapshot("instance/two"));
        configure_value_presentation(&mut app, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            BackendRequest::EnablePrettyPrinting { .. }
        ));
        app.apply_snapshot(v2::Snapshot {
            server_instance_id: "instance/two".into(),
            ..Default::default()
        });
        assert!(app.pretty_printing_attempted.is_empty());
    }

    #[test]
    fn disabled_values_preserve_scope_loading_and_editable_variable_ids() {
        for mode in [
            cli::ValuesFormatting::PrettyPrinters,
            cli::ValuesFormatting::Raw,
            cli::ValuesFormatting::Disabled,
        ] {
            let mut app = App::new("http://localhost".into());
            app.values_formatting = mode;
            app.apply_scopes(vec![v2::Scope {
                scope_id: "scope".into(),
                expensive: true,
                variable_count: Some(1),
                ..Default::default()
            }]);
            assert_eq!(app.toggle_selected_variable().as_deref(), Some("scope"));
            app.apply_variable_children(
                "scope",
                vec![v2::Variable {
                    variable_id: "opaque/value".into(),
                    name: "value".into(),
                    value: "{field = 42}".into(),
                    has_children: true,
                    ..Default::default()
                }],
            );
            app.selected_variable = 1;
            assert_eq!(app.variables[1].id, "opaque/value");
            assert_eq!(app.variables[1].value, "{field = 42}");
            assert_eq!(app.toggle_selected_variable().is_some(), mode.expandable());
            let mut app = distributed_caller_app();
            app.values_formatting = mode;
            app.add_watch("request".into()).unwrap();
            let request = app.prepare_watches().pop().unwrap();
            app.finish_watch(
                request,
                Ok(v2::EvaluationResult {
                    value: "{field = 42}".into(),
                    variable_id: Some("watch/value".into()),
                    has_children: true,
                    ..Default::default()
                }),
            );
            assert_eq!(app.variables[0].id, "watch/value");
            assert_eq!(app.variables[0].has_children, mode.expandable());
        }
    }

    #[test]
    fn source_vim_keys_navigate_and_paging_tracks_viewport_height() {
        let mut app = App::new("http://localhost".into());
        source_browser::tests::show_source(
            &mut app.source_browser,
            v2::SourceContent {
                source: Some(v2::SourceFile {
                    source_reference: "source/main".into(),
                    path: Some("/src/main.rs".into()),
                    ..Default::default()
                }),
                start_line: 1,
                line_count: 40,
                total_lines: 40,
                content: (1..=40)
                    .map(|n| format!("line {n}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                ..Default::default()
            },
            1,
        );
        app.focus = Focus::Source;
        app.areas
            .add_panel(Focus::Source, ratatui::layout::Rect::new(0, 0, 80, 8), 0, 1);
        let (tx, mut rx) = mpsc::channel(4);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().cursor, 2);
        assert_eq!(app.input_mode, InputMode::Normal);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().cursor, 1);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().cursor, 7);
        app.areas.panels[0].area.height = 14;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().cursor, 19);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn page_size_accounts_for_variable_rows_and_tiny_panes() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Variables;
        app.areas.add_panel(
            Focus::Variables,
            ratatui::layout::Rect::new(0, 0, 40, 16),
            0,
            2,
        );
        assert_eq!(page_size(&app), 7);
        app.areas.panels[0].area.height = 1;
        assert_eq!(page_size(&app), 1);
    }

    #[test]
    fn execution_jump_uses_uppercase_key_and_prompt_text_keeps_navigation_letters() {
        let mut app = distributed_caller_app();
        app.capabilities = v2::Capabilities {
            execution_actions: vec![v2::ExecutionAction::Jump as i32],
            execution_action_capabilities: vec![v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Jump as i32,
                scopes: vec![v2::ExecutionScopeKind::Thread as i32],
            }],
            ..Default::default()
        };
        let (tx, mut rx) = mpsc::channel(4);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
            &tx,
        );
        assert_eq!(app.input_mode, InputMode::Jump);
        for character in "junk.rs:12".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                &tx,
            );
        }
        assert_eq!(app.input, "junk.rs:12");
        assert!(rx.try_recv().is_err());
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn source_horizontal_keys_and_mouse_do_not_change_debugger_selection() {
        let mut app = App::new("http://localhost".into());
        source_browser::tests::show_source(
            &mut app.source_browser,
            v2::SourceContent {
                source: Some(v2::SourceFile {
                    source_reference: "source/wide".into(),
                    path: Some("/src/wide.rs".into()),
                    ..Default::default()
                }),
                start_line: 1,
                line_count: 1,
                total_lines: 1,
                content: "x".repeat(100),
                ..Default::default()
            },
            1,
        );
        app.source_browser.fit_horizontal(20);
        app.focus = Focus::Source;
        app.areas.panels.push(model::PanelHitArea {
            focus: Focus::Source,
            area: ratatui::layout::Rect::new(10, 5, 30, 10),
            first_item: 0,
            item_height: 1,
        });
        let (tx, mut rx) = mpsc::channel(4);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().column, 8);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollRight,
                column: 15,
                row: 8,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().column, 16);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.source_browser.document().unwrap().column, 0);
        assert_eq!(app.source_browser.document().unwrap().cursor, 1);
        assert!(app.source_browser.following());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn every_explicit_execution_request_supersedes_automatic_control() {
        let requests_to_test = [
            BackendRequest::Control(Control::Continue, thread_target("thread/child")),
            BackendRequest::Jump {
                location: "main.rs:10".into(),
                target: thread_target("thread/child"),
            },
            BackendRequest::SendSignal {
                signal: "SIGUSR1".into(),
                target: thread_target("thread/child"),
            },
            BackendRequest::RawCommand {
                command: "continue".into(),
                target: thread_target("thread/child"),
                frame_id: None,
            },
        ];
        for request in requests_to_test {
            let mut app = distributed_caller_app();
            app.peer_pauses.insert("session/peer".into());
            app.all_stop_cause = Some("thread/child".into());
            let epoch = app.control_epoch.load(std::sync::atomic::Ordering::SeqCst);
            let (tx, mut rx) = mpsc::channel(1);
            queue_request(&mut app, &tx, request);
            assert_eq!(
                app.control_epoch.load(std::sync::atomic::Ordering::SeqCst),
                epoch + 1
            );
            assert!(app.peer_pauses.is_empty());
            assert!(app.all_stop_cause.is_none());
            assert!(rx.try_recv().is_ok());
        }
    }

    #[test]
    fn inspection_preserves_automatic_control_but_full_queue_execution_cancels_it() {
        let mut app = distributed_caller_app();
        app.peer_pauses.insert("session/peer".into());
        app.all_stop_cause = Some("thread/child".into());
        let (tx, mut rx) = mpsc::channel(1);
        queue_request(
            &mut app,
            &tx,
            BackendRequest::InspectThread {
                thread_id: "thread/child".into(),
                generation: 1,
            },
        );
        assert_eq!(
            app.control_epoch.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(app.peer_pauses.len(), 1);
        queue_request(
            &mut app,
            &tx,
            BackendRequest::Jump {
                location: "main.rs:10".into(),
                target: thread_target("thread/child"),
            },
        );
        assert_eq!(
            app.control_epoch.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(app.peer_pauses.is_empty() && app.all_stop_cause.is_none());
        assert!(app.status.contains("queue is full"));
        assert!(matches!(
            rx.try_recv().unwrap(),
            BackendRequest::InspectThread { .. }
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn rejected_execution_capability_does_not_cancel_automatic_control() {
        let mut app = distributed_caller_app();
        app.peer_pauses.insert("session/peer".into());
        let (tx, mut rx) = mpsc::channel(1);
        dispatch_control(&mut app, &tx, Control::Continue);
        assert_eq!(
            app.control_epoch.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(app.peer_pauses.len(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn source_prompt_returns_focus_from_output_to_source() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Output;
        let (tx, mut rx) = mpsc::channel(4);
        goto_source_line(&mut app, &tx, "main.rs:11");
        assert_eq!(app.focus, Focus::Source);
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::LoadSource(request) if request.line == 11)
        );
    }

    #[test]
    fn output_is_independent_of_timeline_and_keyboard_navigation_is_local() {
        let mut app = App::new("http://localhost".into());
        let (tx, mut rx) = mpsc::channel(4);
        app.output.fit(20, 2);
        apply_message(&mut app, UiMessage::Output("one\ntwo\nthree".into()), &tx);
        assert!(app.timeline.is_empty());
        assert!(!app.show_output && !app.show_timeline);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.focus, Focus::Output);
        assert!(app.show_output && !app.show_timeline);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(
            app.output.visible_lines().collect::<Vec<_>>(),
            ["one", "two"]
        );
        apply_message(&mut app, UiMessage::Output("four".into()), &tx);
        assert_eq!(
            app.output.visible_lines().collect::<Vec<_>>(),
            ["one", "two"]
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(
            app.output.visible_lines().collect::<Vec<_>>(),
            ["three", "four"]
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE),
            &tx,
        );
        assert!(!app.show_output);
        assert_eq!(app.focus, Focus::Source);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn horizontal_inspection_navigation_preserves_selection_and_sends_no_requests() {
        let mut app = distributed_caller_app();
        app.stack_viewport.fit(100, 20);
        app.variable_viewport.fit(120, 20);
        app.focus = Focus::Stack;
        let generation = app.inspection_generation;
        let (tx, mut rx) = mpsc::channel(4);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.stack_viewport.column(), 8);
        app.areas.add_panel(
            Focus::Variables,
            ratatui::layout::Rect::new(10, 5, 30, 10),
            0,
            2,
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollRight,
                column: 15,
                row: 8,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert_eq!(app.variable_viewport.column(), 8);
        assert_eq!(app.stack_viewport.column(), 8);
        assert_eq!(app.focus, Focus::Variables);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.variable_viewport.column(), 0);
        app.topology_viewport.fit(100, 20);
        app.breakpoint_viewport.fit(100, 20);
        for focus in [Focus::Threads, Focus::Breakpoints] {
            app.focus = focus;
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                &tx,
            );
        }
        assert_eq!(app.topology_viewport.column(), 8);
        assert_eq!(app.breakpoint_viewport.column(), 8);
        assert_eq!(app.inspection_generation, generation);
        assert_eq!(app.selected_frame, 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mouse_pane_expansion_is_local_and_help_remains_modal() {
        let mut app = App::new("http://localhost".into());
        app.areas
            .pane_expanders
            .push((Focus::Registers, ratatui::layout::Rect::new(10, 12, 9, 1)));
        let (tx, mut rx) = mpsc::channel(4);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 12,
            modifiers: KeyModifiers::NONE,
        };
        app.show_help = true;
        handle_mouse(&mut app, click, &tx);
        assert!(!app.expanded_pane);
        app.show_help = false;
        handle_mouse(&mut app, click, &tx);
        assert!(app.expanded_pane);
        assert_eq!(app.focus, Focus::Registers);
        handle_mouse(&mut app, click, &tx);
        assert!(!app.expanded_pane);
        assert_eq!(app.focus, Focus::Registers);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn pane_controls_preserve_focus_and_skip_hidden_timeline() {
        let mut app = App::new("http://localhost".into());
        let (tx, _rx) = mpsc::channel(8);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
            &tx,
        );
        assert!(app.expanded_pane);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );
        assert!(!app.expanded_pane);
        assert_eq!(app.focus, Focus::Source);
        for _ in 0..12 {
            app.cycle_focus(false);
            assert_ne!(app.focus, Focus::Timeline);
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            &tx,
        );
        assert!(app.show_timeline);
        assert_eq!(app.focus, Focus::Timeline);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            &tx,
        );
        assert!(!app.show_timeline);
        assert_eq!(app.focus, Focus::Source);
    }

    #[test]
    fn register_keys_scroll_rows_and_values_without_inspection_requests() {
        let mut app = App::new("http://localhost".into());
        app.apply_registers(
            (0..50)
                .map(|n| v2::Register {
                    name: format!("r{n}"),
                    value: "0123456789abcdef".repeat(8),
                    ..Default::default()
                })
                .collect(),
        );
        app.focus = Focus::Registers;
        app.register_viewport.fit(132, 40);
        let (tx, mut rx) = mpsc::channel(8);
        for code in [KeyCode::PageDown, KeyCode::Down, KeyCode::Right] {
            handle_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE), &tx);
        }
        assert_eq!(app.selected_register, 11);
        assert_eq!(app.register_viewport.column(), 8);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.register_viewport.column(), 0);
        app.areas.add_panel(
            Focus::Registers,
            ratatui::layout::Rect::new(10, 5, 30, 10),
            0,
            1,
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollRight,
                column: 15,
                row: 8,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert_eq!(app.register_viewport.column(), 8);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.register_viewport.column(), 0);
        assert_eq!(app.selected_register, 11);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn help_navigation_is_modal_for_keyboard_and_mouse() {
        let mut app = App::new("http://localhost".into());
        let (tx, mut rx) = mpsc::channel(8);
        app.show_help = true;
        app.help_max_scroll = 30;
        app.help_page_height = 5;
        app.areas
            .controls
            .push((Control::Refresh, ratatui::layout::Rect::new(0, 0, 10, 3)));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.help_scroll, 5);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert!(
            rx.try_recv().is_err(),
            "help must not activate the refresh control behind it"
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );
        assert_eq!(app.help_scroll, 8);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &tx,
        );
        assert_eq!(app.help_scroll, 30);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        );
        assert!(!app.show_help);
        assert!(!app.should_quit);
    }

    #[test]
    fn stopped_client_requests_one_pause_for_running_server() {
        let mut app = distributed_caller_app();
        app.threads.push(ThreadItem {
            id: "thread/server".into(),
            session_id: "session/server".into(),
            name: "server".into(),
            state: "running".into(),
            function: String::new(),
            file: None,
            line: None,
        });
        app.capabilities = v2::Capabilities {
            execution_actions: vec![v2::ExecutionAction::Interrupt as i32],
            execution_action_capabilities: vec![v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Interrupt as i32,
                scopes: vec![v2::ExecutionScopeKind::Session as i32],
            }],
            ..Default::default()
        };
        let (tx, mut rx) = mpsc::channel(8);
        for _ in 0..2 {
            apply_message(
                &mut app,
                UiMessage::DebuggerEvent(DebuggerEvent {
                    summary: "breakpoint".into(),
                    refresh: false,
                    activity: DebuggerActivity::Stopped(Some("thread/child".into())),
                }),
                &tx,
            );
        }
        let mut controls = Vec::new();
        while let Ok(request) = rx.try_recv() {
            if !matches!(
                request,
                BackendRequest::InspectThread { .. } | BackendRequest::Refresh
            ) {
                controls.push(request);
            }
        }
        assert_eq!(
            controls.len(),
            1,
            "a client breakpoint must pause the running server exactly once"
        );
        assert!(
            matches!(&controls[0], BackendRequest::PausePeer { session_id, .. } if session_id == "session/server")
        );
        let epoch = app.control_epoch.load(std::sync::atomic::Ordering::SeqCst);
        apply_message(
            &mut app,
            UiMessage::PeerPauseFailed {
                session_id: "session/server".into(),
                epoch,
                error: "denied".into(),
            },
            &tx,
        );
        assert!(!app.peer_pauses.contains("session/server"));
        assert!(app.status.contains("denied"));
        app.threads
            .iter_mut()
            .find(|t| t.id == "thread/server")
            .unwrap()
            .state = "stopped".into();
        apply_message(
            &mut app,
            UiMessage::DebuggerEvent(DebuggerEvent {
                summary: "late peer resume".into(),
                refresh: false,
                activity: DebuggerActivity::Running(Some("thread/server".into())),
            }),
            &tx,
        );
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::PausePeer { session_id, .. } if session_id == "session/server")
        );
    }

    fn distributed_caller_app() -> App {
        let mut app = App::new("http://127.0.0.1:5000".into());
        app.threads.push(ThreadItem {
            id: "thread/child".into(),
            session_id: "session/child".into(),
            name: "child".into(),
            state: "stopped".into(),
            function: "leaf".into(),
            file: None,
            line: None,
        });
        app.begin_inspection();
        app.frames.push(model::StackFrame {
            id: "frame/parent".into(),
            distributed_index: 2,
            session_id: "session/parent".into(),
            thread_id: "thread/parent".into(),
            level: 0,
            address: String::new(),
            function: "caller".into(),
            file: Some("parent.rs".into()),
            line: Some(10),
            boundary: false,
        });
        app
    }

    #[test]
    fn a_new_stop_revision_refreshes_inspection_without_a_running_event() {
        let mut app = distributed_caller_app();
        let (requests, mut receiver) = mpsc::channel(4);
        let mut snapshot = v2::Snapshot {
            sessions: vec![v2::Session {
                session_id: "session/child".into(),
                ..Default::default()
            }],
            execution_states: vec![v2::ExecutionState {
                execution_state_id: "execution/child".into(),
                target: Some(thread_target("thread/child")),
                running: false,
                revision: 10,
                location: Some(v2::SourceLocation {
                    path: Some("child.rs".into()),
                    line: 37,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        app.apply_snapshot(snapshot.clone());
        let generation = app.inspection_generation;
        // Same source location: comparing lines alone would miss new variable values.
        snapshot.execution_states[0].revision = 12;
        apply_message(&mut app, UiMessage::Snapshot(snapshot.clone()), &requests);
        assert!(
            matches!(receiver.try_recv().unwrap(), BackendRequest::InspectThread {
            thread_id, generation: next,
        } if thread_id == "thread/child" && next > generation)
        );
        assert!(app.frames.is_empty());
        apply_message(&mut app, UiMessage::Snapshot(snapshot), &requests);
        assert!(
            receiver.try_recv().is_err(),
            "duplicate stop snapshots must not restart inspection"
        );
    }

    #[test]
    fn an_uninspected_threads_stop_revision_does_not_restart_inspection() {
        let mut app = distributed_caller_app();
        let (requests, mut receiver) = mpsc::channel(1);
        let mut snapshot = v2::Snapshot {
            sessions: vec![v2::Session {
                session_id: "session/child".into(),
                ..Default::default()
            }],
            execution_states: vec![v2::ExecutionState {
                execution_state_id: "execution/other".into(),
                target: Some(thread_target("thread/other")),
                revision: 10,
                ..Default::default()
            }],
            ..Default::default()
        };
        app.apply_snapshot(snapshot.clone());
        let generation = app.inspection_generation;
        snapshot.execution_states[0].revision = 12;
        apply_message(&mut app, UiMessage::Snapshot(snapshot), &requests);
        assert_eq!(app.inspection_generation, generation);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn refreshed_stack_selection_matches_the_workers_inspected_frame() {
        let mut app = distributed_caller_app();
        app.selected_frame = 2;
        app.apply_distributed_frames(vec![
            v2::DistributedFrame::default(),
            v2::DistributedFrame {
                frame: Some(v2::Frame {
                    frame_id: "first".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            v2::DistributedFrame {
                frame: Some(v2::Frame {
                    frame_id: "second".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ]);
        assert_eq!(app.current_frame().unwrap().id, "first");
    }

    #[test]
    fn a_running_distributed_caller_invalidates_its_inspection_handles() {
        let mut app = distributed_caller_app();
        let generation = app.inspection_generation;
        app.mark_running(Some("thread/parent"));
        assert!(app.frames.is_empty());
        assert!(app.inspection_generation > generation);
    }

    #[test]
    fn source_navigation_does_not_cancel_variable_expansion() {
        let mut app = distributed_caller_app();
        app.apply_scopes(vec![v2::Scope {
            scope_id: "locals".into(),
            ..Default::default()
        }]);
        assert!(app.variables[0].loading);
        let generation = app.inspection_generation;
        let (requests, mut receiver) = mpsc::channel(1);
        goto_source_line(&mut app, &requests, "parent.rs:500");
        assert!(matches!(
            receiver.try_recv().unwrap(),
            BackendRequest::LoadSource(_)
        ));
        assert!(app.variables[0].loading);
        assert_eq!(app.inspection_generation, generation);
        assert!(app.toggle_selected_variable().is_none());
    }

    #[test]
    fn evaluation_targets_the_distributed_frames_owner() {
        let mut app = distributed_caller_app();
        let (requests, mut receiver) = mpsc::channel(1);
        app.start_input(InputMode::Evaluate);
        app.input = "value".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &requests,
        );
        assert!(
            matches!(receiver.try_recv().unwrap(), BackendRequest::Evaluate {
            thread_id, frame_id: Some(frame_id), expression,
        } if thread_id == "thread/parent" && frame_id == "frame/parent" && expression == "value")
        );

        app.frames[0].boundary = true;
        app.start_input(InputMode::Evaluate);
        app.input = "value".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &requests,
        );
        assert!(receiver.try_recv().is_err());
        assert!(app.status.contains("inspectable frame"));
    }

    #[test]
    fn manual_source_navigation_does_not_require_a_frame_or_thread() {
        let mut app = App::new("http://localhost".into());
        let (requests, mut receiver) = mpsc::channel(1);
        goto_source_line(&mut app, &requests, "parent.rs:500");
        assert!(
            matches!(receiver.try_recv().unwrap(), BackendRequest::LoadSource(request)
                if request.line == 500 && matches!(&request.address, source_types::Address::Resolve { target: None, location, .. } if location.path.as_deref() == Some("parent.rs")))
        );
        assert_eq!(app.inspection_generation, 0);
    }

    #[test]
    fn selecting_a_stopped_thread_restores_source_following_after_manual_browsing() {
        let mut app = distributed_caller_app();
        app.source_browser.set_following(false);
        app.focus = Focus::Threads;
        let (tx, mut rx) = mpsc::channel(4);
        inspect_selection(&mut app, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            BackendRequest::InspectThread { .. }
        ));
        assert!(
            app.source_browser.following(),
            "explicit thread selection must reveal its next frame response"
        );
    }

    #[test]
    fn frame_shortcut_reveals_distributed_owner_without_changing_execution_target() {
        let mut app = distributed_caller_app();
        app.focus = Focus::Variables;
        app.source_browser.set_following(false);
        app.execution_target = Some(thread_target("thread/child"));
        let target = app.execution_target.clone();
        let generation = app.inspection_generation;
        let (requests, mut receiver) = mpsc::channel(4);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE),
            &requests,
        );
        assert_eq!(app.focus, Focus::Source);
        assert_eq!(app.execution_target, target);
        assert_eq!(app.inspection_generation, generation);
        assert!(
            matches!(receiver.try_recv().unwrap(), BackendRequest::LoadSource(request)
            if request.line == 10 && matches!(&request.address,
                source_types::Address::Resolve { target: Some(target), location, .. }
                if *target == thread_target("thread/parent") && location.path.as_deref() == Some("parent.rs")))
        );
        app.frames[0].file = None;
        reveal_selected_frame(&mut app, &requests);
        assert!(receiver.try_recv().is_err());
        assert!(app.status.contains("Source unavailable"));
        app.frames[0].boundary = true;
        reveal_selected_frame(&mut app, &requests);
        assert!(receiver.try_recv().is_err());
        assert!(app.status.contains("boundary"));
    }

    #[test]
    fn native_console_uses_selected_distributed_frame_and_preserves_command_text() {
        let mut app = distributed_caller_app();
        let (tx, mut rx) = mpsc::channel(4);
        app.start_input(InputMode::Command);
        app.input = "print value + 1".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::RawCommand { command, target, frame_id }
            if command == "print value + 1" && target == thread_target("thread/parent") && frame_id.as_deref() == Some("frame/parent"))
        );
        app.frames[0].boundary = true;
        app.start_input(InputMode::Command);
        app.input = "print value".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(app.status.contains("inspectable frame"));
    }

    #[test]
    fn boundary_selection_clears_inspection_and_rejects_late_replies() {
        let mut app = distributed_caller_app();
        app.add_watch("request".into()).unwrap();
        let watch = app.prepare_watches().pop().unwrap();
        app.finish_watch(
            watch.clone(),
            Ok(v2::EvaluationResult {
                value: "old frame value".into(),
                variable_id: Some("old/handle".into()),
                ..Default::default()
            }),
        );
        let generation = app.inspection_generation;
        let (requests, mut receiver) = mpsc::channel(4);
        let registers = || UiMessage::Registers {
            generation,
            thread_id: "thread/child".into(),
            registers: vec![v2::Register::default()],
        };
        apply_message(&mut app, registers(), &requests);
        assert_eq!(app.registers.len(), 1);
        app.frames[0].boundary = true;
        app.focus = Focus::Stack;
        inspect_selection(&mut app, &requests);
        assert!(app.registers.is_empty());
        assert!(app.variables[0].id.is_empty());
        assert_eq!(app.watches.len(), 1);
        assert!(app.source_browser.error().unwrap().contains("boundary"));
        assert_eq!(app.focus, Focus::Stack);
        assert!(receiver.try_recv().is_err());
        apply_message(&mut app, registers(), &requests);
        app.finish_watch(
            watch,
            Ok(v2::EvaluationResult {
                value: "late old value".into(),
                ..Default::default()
            }),
        );
        assert!(app.registers.is_empty());
        assert_eq!(app.variables[0].value, "Waiting for a stopped frame");
        app.frames[0].boundary = false;
        inspect_selection(&mut app, &requests);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            BackendRequest::LoadSource(_)
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            BackendRequest::InspectFrame { .. }
        ));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            BackendRequest::EvaluateWatch { .. }
        ));
    }

    #[test]
    fn watches_keep_caller_ownership_and_reject_old_revisions_and_frames() {
        let mut app = distributed_caller_app();
        app.add_watch("request".into()).unwrap();
        let first = app.prepare_watches().pop().unwrap();
        assert_eq!(first.owner_thread, "thread/parent");
        assert_eq!(first.origin_thread, "thread/child");
        app.finish_watch(
            first.clone(),
            Ok(v2::EvaluationResult {
                value: "request value".into(),
                variable_id: Some("retained/request".into()),
                has_children: true,
                ..Default::default()
            }),
        );
        app.apply_scopes(vec![]);
        assert_eq!(app.variables[0].id, "retained/request");
        assert_eq!(
            app.toggle_selected_variable().as_deref(),
            Some("retained/request")
        );
        app.apply_variable_children(
            "retained/request",
            vec![v2::Variable {
                variable_id: "retained/child".into(),
                name: "child".into(),
                ..Default::default()
            }],
        );
        // Scope responses can arrive after a retained watch has expanded.
        app.apply_scopes(vec![]);
        assert_eq!(app.variables.len(), 2);
        let second = app.prepare_watches().pop().unwrap();
        app.finish_watch(first, Err("old failure".into()));
        assert_eq!(app.variables[0].value, "Evaluating…");
        app.finish_watch(second.clone(), Err("not in scope".into()));
        assert!(app.variables[0].value.contains("not in scope"));
        app.begin_frame_inspection();
        app.finish_watch(
            second,
            Ok(v2::EvaluationResult {
                value: "stale".into(),
                ..Default::default()
            }),
        );
        assert!(app.variables[0].id.is_empty());
        assert_eq!(app.variables[0].value, "Waiting for a stopped frame");
        let third = app.prepare_watches().pop().unwrap();
        assert!(app.remove_selected_watch());
        app.finish_watch(third, Ok(v2::EvaluationResult::default()));
        assert!(app.variables.is_empty());
        assert!(app.watches.is_empty());
    }

    #[test]
    fn variable_assignment_captures_owner_and_rejects_changed_inspection() {
        let mut app = distributed_caller_app();
        app.apply_variables(vec![v2::Variable {
            variable_id: "var/parent".into(),
            name: "value".into(),
            value: "1".into(),
            ..Default::default()
        }]);
        app.focus = Focus::Variables;
        let (tx, mut rx) = mpsc::channel(4);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('='), KeyModifiers::NONE),
            &tx,
        );
        app.input = "42".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::AssignVariable { edit, value }
            if edit.variable_id == "var/parent" && edit.owner_thread == "thread/parent" && edit.origin_thread == "thread/child" && value == "42")
        );
        start_variable_edit(&mut app);
        app.inspection_generation += 1;
        app.input = "99".into();
        handle_prompt_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &tx,
        );
        assert!(rx.try_recv().is_err());
        assert!(app.status.contains("context changed"));
    }

    #[test]
    fn variable_assignment_refreshes_its_frame_and_ignores_stale_results() {
        let mut app = distributed_caller_app();
        let edit = model::VariableEdit {
            variable_id: "var/parent".into(),
            owner_thread: "thread/parent".into(),
            origin_thread: "thread/child".into(),
            frame_id: app.current_frame().unwrap().id.clone(),
            generation: app.inspection_generation,
        };
        let (tx, mut rx) = mpsc::channel(4);
        apply_message(
            &mut app,
            UiMessage::VariableAssigned {
                edit: edit.clone(),
                result: Err("Variable is read-only".into()),
            },
            &tx,
        );
        assert!(app.status.contains("read-only"));
        assert!(rx.try_recv().is_err());
        assert_eq!(app.inspection_generation, edit.generation);

        apply_message(
            &mut app,
            UiMessage::VariableAssigned {
                edit: edit.clone(),
                result: Ok(v2::Operation {
                    state: v2::OperationState::Completed as i32,
                    ..Default::default()
                }),
            },
            &tx,
        );
        assert!(
            matches!(rx.try_recv().unwrap(), BackendRequest::InspectFrame {
            thread_id, generation, frame_id
        } if thread_id == edit.origin_thread && frame_id == edit.frame_id
            && generation == app.inspection_generation && generation > edit.generation)
        );
        assert_eq!(app.current_frame().unwrap().id, edit.frame_id);
        let status = app.status.clone();
        apply_message(
            &mut app,
            UiMessage::VariableAssigned {
                edit,
                result: Err("stale failure".into()),
            },
            &tx,
        );
        assert_eq!(app.status, status);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_full_request_queue_does_not_leave_a_scope_loading_forever() {
        let mut app = distributed_caller_app();
        let (requests, mut receiver) = mpsc::channel(1);
        requests.try_send(BackendRequest::Refresh).unwrap();
        app.apply_scopes(vec![v2::Scope {
            scope_id: "globals".into(),
            expensive: true,
            ..Default::default()
        }]);
        toggle_selected_variable(&mut app, &requests);
        assert!(!app.variables[0].loading);
        receiver.try_recv().unwrap();
        toggle_selected_variable(&mut app, &requests);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            BackendRequest::ExpandScope { .. }
        ));
    }

    #[test]
    fn scope_expansion_is_lazy_retryable_and_generation_scoped() {
        let mut app = distributed_caller_app();
        let (requests, mut receiver) = mpsc::channel(1);
        app.apply_scopes(vec![v2::Scope {
            scope_id: "scope/globals".into(),
            name: "Globals".into(),
            expensive: true,
            ..Default::default()
        }]);
        toggle_selected_variable(&mut app, &requests);
        assert!(
            matches!(receiver.try_recv().unwrap(), BackendRequest::ExpandScope {
            scope_id, generation, thread_id,
        } if scope_id == "scope/globals" && generation == app.inspection_generation && thread_id == "thread/child")
        );
        toggle_selected_variable(&mut app, &requests);
        assert!(
            receiver.try_recv().is_err(),
            "do not duplicate an in-flight request"
        );

        let generation = app.inspection_generation;
        apply_message(
            &mut app,
            UiMessage::ExpansionError {
                generation: generation - 1,
                thread_id: "thread/child".into(),
                parent_id: "scope/globals".into(),
                error: "stale".into(),
            },
            &requests,
        );
        assert!(app.variables[0].loading);
        apply_message(
            &mut app,
            UiMessage::ExpansionError {
                generation,
                thread_id: "thread/child".into(),
                parent_id: "scope/globals".into(),
                error: "unavailable".into(),
            },
            &requests,
        );
        assert!(!app.variables[0].loading);
        assert_eq!(app.inspection_generation, generation);
        toggle_selected_variable(&mut app, &requests);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            BackendRequest::ExpandScope { .. }
        ));
    }

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
    fn breakpoint_ignore_counts_are_explicit_and_checked() {
        let options = parse_breakpoint_options("--ignore-count 3 -t if x == 4").unwrap();
        assert_eq!(options.ignore_count, Some(3));
        assert!(options.temporary);
        assert_eq!(options.condition.as_deref(), Some("x == 4"));
        for invalid in ["", "-1", "+1", "1.5", "18446744073709551616"] {
            assert!(parse_breakpoint_options(&format!("--ignore-count {invalid}")).is_err());
        }
        assert!(parse_breakpoint_options("--ignore-count 1 --ignore-count 2").is_err());
        for count in [0, u64::MAX] {
            assert_eq!(
                parse_breakpoint_options(&format!("--ignore-count {count}"))
                    .unwrap()
                    .ignore_count,
                Some(count)
            );
        }
        let (name, options) =
            parse_function_breakpoint("Widget::operator new(unsigned long) -- --ignore-count 2 -t")
                .unwrap();
        assert_eq!(name, "Widget::operator new(unsigned long)");
        assert_eq!(options.ignore_count, Some(2));
        assert!(options.temporary);
        assert_eq!(
            parse_function_breakpoint("Widget::operator new(unsigned long)")
                .unwrap()
                .1,
            BreakpointOptions::default()
        );
    }

    #[test]
    fn parses_advanced_breakpoint_options_and_condition() {
        assert_eq!(
            parse_breakpoint_options("-t if request.id == 42").unwrap(),
            BreakpointOptions {
                condition: Some("request.id == 42".to_string()),
                ignore_count: None,
                temporary: true,
                hardware: false,
            }
        );
        assert_eq!(
            parse_breakpoint_options("--hardware -c ptr == 0").unwrap(),
            BreakpointOptions {
                condition: Some("ptr == 0".to_string()),
                ignore_count: None,
                temporary: false,
                hardware: true,
            }
        );
        assert_eq!(
            parse_breakpoint_options("-t -h").unwrap(),
            BreakpointOptions {
                condition: None,
                ignore_count: None,
                temporary: true,
                hardware: true,
            }
        );
    }
}
