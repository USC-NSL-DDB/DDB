use super::*;
use crate::source_explorer::{Control as ExplorerControl, Entry, Place};

pub(super) fn draw(frame: &mut Frame<'_>, app: &App, root: Rect, areas: &mut UiAreas) {
    let explorer = &app.source_explorer;
    if !explorer.visible {
        return;
    }
    let area = centered(root, 94, 90);
    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Source explorer ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .split(inner);
    let controls = [
        (ExplorerControl::Roots, "Roots"),
        (ExplorerControl::Catalog, "Debug sources"),
        (ExplorerControl::Parent, "Up"),
        (ExplorerControl::Refresh, "Refresh"),
        (
            ExplorerControl::Hidden,
            if explorer.include_hidden {
                "Hide dotfiles"
            } else {
                "Dotfiles"
            },
        ),
    ];
    let close_width = rows[0].width.min(7);
    let close = Rect::new(
        rows[0].right().saturating_sub(close_width),
        rows[0].y,
        close_width,
        rows[0].height,
    );
    frame.render_widget(
        Paragraph::new(" Close ").style(Style::default().fg(ACCENT)),
        close,
    );
    areas
        .source_explorer_controls
        .push((ExplorerControl::Close, close));
    let mut x = rows[0].x;
    for (control, text) in controls {
        let width = (text.len() as u16 + 2).min(close.x.saturating_sub(x));
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, rows[0].y, width, rows[0].height);
        frame.render_widget(
            Paragraph::new(format!(" {text} ")).style(Style::default().fg(ACCENT)),
            rect,
        );
        areas.source_explorer_controls.push((control, rect));
        x = x.saturating_add(width);
    }
    let path = match &explorer.place {
        Place::Roots => "DDB workspace roots".to_owned(),
        Place::Catalog => "Debugger source catalog".to_owned(),
        Place::Directory { root, path } => format!("DDB · {}/{path}", root.name),
    };
    let filter = if explorer.place == Place::Catalog {
        "search"
    } else {
        "filter page"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{}  | {filter}: {}",
            terminal_text(&path),
            terminal_text(&explorer.query)
        )),
        rows[1],
    );
    let mut labels = Vec::new();
    if explorer.has_previous() {
        labels.push("‹ Previous page".to_owned());
    }
    for index in explorer.visible_indices() {
        let entry = &explorer.entries[index];
        let (kind, owners, unavailable) = match entry {
            Entry::Root(root) => ("[root]", root.owners.as_slice(), false),
            Entry::Directory(_) => ("[dir]", &[][..], false),
            Entry::Link(_) => ("[link]", &[][..], false),
            Entry::Other(_) => ("[other]", &[][..], false),
            Entry::Source(source) => (
                if source.origin == v2::SourceOrigin::Debugger as i32 {
                    "[debug]"
                } else {
                    "[file]"
                },
                source.owners.as_slice(),
                source.availability == v2::SourceAvailability::Unavailable as i32,
            ),
        };
        let owners = owners
            .iter()
            .map(|owner| {
                app.sessions
                    .iter()
                    .find(|session| session.session_id == owner.session_id)
                    .map(|session| session.display_name.as_str())
                    .unwrap_or(&owner.session_id)
            })
            .collect::<Vec<_>>()
            .join(", ");
        labels.push(format!(
            "{kind} {}{}{}",
            terminal_text(entry.label()),
            if owners.is_empty() {
                String::new()
            } else {
                format!("  [{}]", terminal_text(&owners))
            },
            if unavailable { "  [unavailable]" } else { "" }
        ));
    }
    if explorer.has_next() {
        labels.push("Next page ›".to_owned());
    }
    let height = rows[2].height as usize;
    let first = explorer.selected.saturating_add(1).saturating_sub(height);
    if explorer.loading() {
        frame.render_widget(
            Paragraph::new("Loading sources…").style(Style::default().fg(MUTED)),
            rows[2],
        );
    } else if labels.is_empty() {
        let empty = match explorer.place {
            Place::Roots => "No exported roots. Configure SourceRoots in the DDB startup file; Tab opens debugger sources.",
            Place::Catalog => "No matching debugger sources. Try a shorter search or refresh after stopping a target.",
            Place::Directory { .. } => "No matching entries on this page.",
        };
        frame.render_widget(
            Paragraph::new(empty)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(MUTED)),
            rows[2],
        );
    } else {
        let items = labels
            .iter()
            .skip(first)
            .take(height)
            .map(|text| ListItem::new(text.as_str()))
            .collect::<Vec<_>>();
        let visible = items.len();
        let mut state =
            ListState::default().with_selected(Some(explorer.selected.saturating_sub(first)));
        frame.render_stateful_widget(
            List::new(items).highlight_symbol("▸ ").highlight_style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(38, 79, 120)),
            ),
            rows[2],
            &mut state,
        );
        areas
            .source_explorer_rows
            .extend((0..visible).map(|offset| {
                (
                    first + offset,
                    Rect::new(
                        rows[2].x,
                        rows[2].y.saturating_add(offset as u16),
                        rows[2].width,
                        1,
                    ),
                )
            }));
    }
    let selected = explorer.selected_entry();
    let unavailable = match selected {
        Some(Entry::Source(source)) => source.unavailable_reason.as_deref(),
        _ => None,
    };
    let detail = explorer
        .error
        .as_deref()
        .or(unavailable)
        .or_else(|| explorer.warnings.first().map(String::as_str))
        .unwrap_or(if matches!(selected, Some(Entry::Link(_))) {
            "Link: Enter/click opens as a file; Right opens as a directory. Access stays within the exported root."
        } else {
            "Browsing does not change the selected debugger frame or execution target."
        });
    frame.render_widget(
        Paragraph::new(terminal_text(detail))
            .wrap(Wrap { trim: false })
            .style(
                Style::default().fg(if explorer.error.is_some() || unavailable.is_some() {
                    YELLOW
                } else {
                    MUTED
                }),
            ),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new(
            "Enter/click open · Tab view · Left parent · F5 refresh · Ctrl-H dotfiles · Esc close",
        )
        .style(Style::default().fg(MUTED)),
        rows[4],
    );
}
