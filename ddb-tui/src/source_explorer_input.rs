use super::*;
use crate::source_explorer::{Action, Control as ExplorerControl};

pub(super) fn handle_key(app: &mut App, key: KeyEvent, requests: &mpsc::Sender<BackendRequest>) {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => apply_control(app, requests, ExplorerControl::Close),
        KeyCode::Tab | KeyCode::BackTab => {
            let request = app.source_explorer.switch_view();
            queue_request(app, requests, BackendRequest::ExploreSources(request));
        }
        KeyCode::F(5) => apply_control(app, requests, ExplorerControl::Refresh),
        KeyCode::Char('h') if control => apply_control(app, requests, ExplorerControl::Hidden),
        KeyCode::Char('u') if control => {
            if let Some(request) = app.source_explorer.clear_query() {
                queue_request(app, requests, BackendRequest::ExploreSources(request));
            }
        }
        KeyCode::Left => apply_control(app, requests, ExplorerControl::Parent),
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            apply_control(app, requests, ExplorerControl::Parent)
        }
        KeyCode::Up => app.source_explorer.move_selection(-1),
        KeyCode::Down => app.source_explorer.move_selection(1),
        KeyCode::PageUp => app
            .source_explorer
            .move_selection(-(app.areas.source_explorer_rows.len().max(1) as isize)),
        KeyCode::PageDown => app
            .source_explorer
            .move_selection(app.areas.source_explorer_rows.len().max(1) as isize),
        KeyCode::Home => app.source_explorer.select(0),
        KeyCode::End => app.source_explorer.select(usize::MAX),
        KeyCode::Backspace => {
            if let Some(request) = app.source_explorer.edit_query(None) {
                queue_request(app, requests, BackendRequest::ExploreSources(request));
            }
        }
        KeyCode::Enter => activate(app, requests, false),
        KeyCode::Right => activate(app, requests, true),
        KeyCode::Char(ch) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
            edit_query(app, requests, &ch.to_string())
        }
        _ => {}
    }
}

pub(super) fn edit_query(app: &mut App, requests: &mpsc::Sender<BackendRequest>, text: &str) {
    if let Some(request) = app.source_explorer.edit_query(Some(text)) {
        queue_request(app, requests, BackendRequest::ExploreSources(request));
    }
}

pub(super) fn handle_mouse(
    app: &mut App,
    mouse: MouseEvent,
    requests: &mpsc::Sender<BackendRequest>,
) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(control) = app
                .areas
                .source_explorer_control_at(mouse.column, mouse.row)
            {
                apply_control(app, requests, control);
            } else if let Some(row) = app.areas.source_explorer_at(mouse.column, mouse.row) {
                app.source_explorer.select(row);
                activate(app, requests, false);
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            apply_control(app, requests, ExplorerControl::Parent)
        }
        MouseEventKind::ScrollUp => app.source_explorer.move_selection(-3),
        MouseEventKind::ScrollDown => app.source_explorer.move_selection(3),
        _ => {}
    }
}

fn apply_control(app: &mut App, requests: &mpsc::Sender<BackendRequest>, control: ExplorerControl) {
    let request = match control {
        ExplorerControl::Close => {
            app.source_explorer.close();
            queue_request(app, requests, BackendRequest::CancelSourceDiscovery);
            None
        }
        ExplorerControl::Roots => Some(app.source_explorer.open(false)),
        ExplorerControl::Catalog => Some(app.source_explorer.open(true)),
        ExplorerControl::Parent => app.source_explorer.parent(),
        ExplorerControl::Refresh => Some(app.source_explorer.refresh()),
        ExplorerControl::Hidden => app.source_explorer.toggle_hidden(),
    };
    if let Some(request) = request {
        queue_request(app, requests, BackendRequest::ExploreSources(request));
    }
}

fn activate(app: &mut App, requests: &mpsc::Sender<BackendRequest>, directory: bool) {
    let action = if directory {
        app.source_explorer.enter_directory()
    } else {
        app.source_explorer.activate()
    };
    let Some(action) = action else {
        return;
    };
    let request = match action {
        Action::Request(request) => {
            queue_request(app, requests, BackendRequest::ExploreSources(request));
            return;
        }
        Action::Open(source) => app.source_browser.open_source(source),
        Action::Resolve { root, path } => app.source_browser.open_export(root, path),
    };
    app.source_explorer.close();
    app.focus = Focus::Source;
    queue_request(app, requests, BackendRequest::LoadSource(request));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_browser::tests::remote_source;
    use crate::source_explorer::{Entry, Page};

    #[test]
    fn paging_uses_visible_rows_after_resize_without_backend_requests() {
        let mut app = App::new("http://localhost".into());
        let request = app.source_explorer.open(true);
        app.source_explorer.apply(
            request.generation,
            Page {
                entries: (0..30)
                    .map(|index| {
                        Entry::Source(remote_source(v2::SourceFile {
                            source_reference: format!("source/{index}"),
                            name: format!("file{index}.rs"),
                            ..Default::default()
                        }))
                    })
                    .collect(),
                next_token: None,
                warnings: Vec::new(),
            },
        );
        app.source_explorer.select(10);
        let (tx, mut rx) = mpsc::channel(1);
        for (rows, code, expected) in [
            (3, KeyCode::PageDown, 13),
            (3, KeyCode::PageUp, 10),
            (7, KeyCode::PageDown, 17),
            (0, KeyCode::PageDown, 18),
        ] {
            app.areas.source_explorer_rows = (0..rows)
                .map(|index| (index, ratatui::layout::Rect::new(0, index as u16, 20, 1)))
                .collect();
            handle_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE), &tx);
            assert_eq!(app.source_explorer.selected, expected);
        }
        assert!(rx.try_recv().is_err());
    }
}
