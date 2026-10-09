#[path = "source_explorer_ui.rs"]
mod explorer;

use ddb_api_client::v2::{self, target};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
    Frame,
};

use crate::{
    api::CapabilitiesExt,
    display_text::{horizontal_line, horizontal_text, terminal_text},
    model::{App, Control, Focus, InputMode, TopologyRowKind, UiAreas},
};

const ACCENT: Color = Color::Rgb(86, 156, 214);
const GREEN: Color = Color::Rgb(78, 201, 176);
const YELLOW: Color = Color::Rgb(220, 220, 170);
const RED: Color = Color::Rgb(244, 71, 71);
const MUTED: Color = Color::Rgb(128, 128, 128);

pub fn draw(frame: &mut Frame<'_>, app: &mut App) -> UiAreas {
    let root = frame.area();
    if root.height < 19 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(root);
        let mut areas = UiAreas::default();
        draw_mini_toolbar(frame, app, rows[0]);
        draw_compact_panel(frame, app, rows[1], &mut areas);
        draw_pane_expanders(frame, app, &mut areas);
        draw_mini_status(frame, app, rows[2]);
        if app.show_help {
            draw_help(frame, app, root);
        }
        draw_breakpoint_target_picker(frame, app, root, &mut areas);
        draw_signal_picker(frame, app, root, &mut areas);
        draw_breakpoint_hits(frame, app, root, &mut areas);
        explorer::draw(frame, app, root, &mut areas);
        return areas;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(root);

    let mut areas = UiAreas::default();
    draw_toolbar(frame, app, rows[0], &mut areas);

    if app.expanded_pane || root.width < 100 || root.height < 28 {
        draw_compact_panel(frame, app, rows[1], &mut areas);
        draw_pane_expanders(frame, app, &mut areas);
        draw_prompt(frame, app, rows[2]);
        draw_hints(frame, rows[3]);
        if app.show_help {
            draw_help(frame, app, root);
        }
        draw_signal_picker(frame, app, root, &mut areas);
        draw_breakpoint_hits(frame, app, root, &mut areas);
        draw_breakpoint_target_picker(frame, app, root, &mut areas);
        explorer::draw(frame, app, root, &mut areas);
        return areas;
    }

    let body = if app.show_output {
        let split = Layout::vertical([Constraint::Min(6), Constraint::Length(9)]).split(rows[1]);
        draw_output(frame, app, split[1]);
        areas.add_focus_panel(Focus::Output, split[1]);
        split[0]
    } else {
        rows[1]
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(24),
            Constraint::Percentage(51),
            Constraint::Percentage(25),
        ])
        .split(body);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(columns[0]);
    let center = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(columns[1]);
    let right = if app.extension_panels.is_empty() && app.extension_actions.is_empty() {
        Layout::vertical([
            Constraint::Percentage(if app.show_timeline { 65 } else { 100 }),
            Constraint::Percentage(if app.show_timeline { 35 } else { 0 }),
        ])
        .split(columns[2])
    } else {
        Layout::vertical([
            Constraint::Percentage(if app.show_timeline { 48 } else { 65 }),
            Constraint::Percentage(if app.show_timeline { 32 } else { 35 }),
            Constraint::Percentage(if app.show_timeline { 20 } else { 0 }),
        ])
        .split(columns[2])
    };

    draw_threads(frame, app, left[0]);
    draw_breakpoints(frame, app, left[1], &mut areas);
    draw_source(frame, app, center[0], &mut areas);
    draw_stack(frame, app, center[1]);
    draw_variables(frame, app, right[0]);
    if app.extension_panels.is_empty() && app.extension_actions.is_empty() {
        if app.show_timeline {
            draw_timeline(frame, app, right[1]);
        }
    } else {
        draw_extensions(frame, app, right[1], &mut areas);
        if app.show_timeline {
            draw_timeline(frame, app, right[2]);
        }
    }
    areas.add_panel(
        Focus::Threads,
        left[0],
        list_first(
            app.selected_topology_row(),
            app.topology_rows().len(),
            left[0],
            1,
        ),
        1,
    );
    areas.add_panel(Focus::Source, center[0], source_start(app), 1);
    areas.add_panel(
        Focus::Stack,
        center[1],
        list_first(app.selected_frame, app.frames.len(), center[1], 1),
        1,
    );
    let variables_area = variable_list_area(app, right[0]);
    register_hit_area(app, right[0], &mut areas);
    areas.add_panel(
        Focus::Variables,
        variables_area,
        list_first(
            app.selected_variable,
            app.variables.len(),
            variables_area,
            2,
        ),
        2,
    );
    if app.extension_panels.is_empty() && app.extension_actions.is_empty() {
        if app.show_timeline {
            areas.add_focus_panel(Focus::Timeline, right[1]);
        }
    } else {
        areas.add_focus_panel(Focus::Extensions, right[1]);
        if app.show_timeline {
            areas.add_focus_panel(Focus::Timeline, right[2]);
        }
    }

    draw_pane_expanders(frame, app, &mut areas);
    draw_prompt(frame, app, rows[2]);
    draw_hints(frame, rows[3]);
    if app.show_help {
        draw_help(frame, app, root);
    }
    draw_breakpoint_target_picker(frame, app, root, &mut areas);
    draw_signal_picker(frame, app, root, &mut areas);
    draw_breakpoint_hits(frame, app, root, &mut areas);
    explorer::draw(frame, app, root, &mut areas);
    areas
}

fn draw_pane_expanders(frame: &mut Frame<'_>, app: &App, areas: &mut UiAreas) {
    for panel in &areas.panels {
        let label = if app.expanded_pane && app.focus == panel.focus {
            "[Restore]"
        } else {
            "[Expand]"
        };
        let width = label.len() as u16;
        if panel.area.width < width + 4 || panel.area.height < 3 {
            continue;
        }
        let button = Rect::new(
            panel.area.right() - width - 1,
            panel.area.bottom() - 1,
            width,
            1,
        );
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(ACCENT)),
            button,
        );
        areas.pane_expanders.push((panel.focus, button));
    }
}

fn draw_breakpoint_hits(frame: &mut Frame<'_>, app: &App, root: Rect, areas: &mut UiAreas) {
    let Some(picker) = app.breakpoint_hit_picker.as_ref() else {
        return;
    };
    let area = centered(root, 94, 80);
    frame.render_widget(Clear, area);
    let block = panel_block("Paused breakpoint hits · select a thread", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
    let height = rows[0].height as usize;
    let first = picker.cursor.saturating_sub(height.saturating_sub(1));
    let items = picker
        .hits
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(index, hit)| {
            let name = app
                .threads
                .iter()
                .find(|thread| thread.id == hit.thread_id)
                .map(|thread| thread.name.as_str())
                .unwrap_or(&hit.thread_id);
            let label = format!(
                "{} · {} · {} · stop {}",
                session_display_name(app, &hit.session_id),
                name,
                hit.thread_id,
                hit.revision
            );
            areas.breakpoint_hit_rows.push((
                index,
                Rect::new(
                    rows[0].x,
                    rows[0].y + (index - first) as u16,
                    rows[0].width,
                    1,
                ),
            ));
            ListItem::new(terminal_text(&label))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(picker.cursor - first));
    }
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("▸ ")
            .highlight_style(Style::default().bg(Color::Rgb(38, 79, 120))),
        rows[0],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new("[Cancel]  Enter/click opens frame · arrows/j/k select"),
        rows[1],
    );
    areas.breakpoint_hit_cancel = Some(Rect::new(
        rows[1].x,
        rows[1].y,
        rows[1].width.min(8),
        rows[1].height,
    ));
}

fn draw_mini_toolbar(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let state = if app.api_connected {
        "connected"
    } else {
        "reconnecting"
    };
    frame.render_widget(
        Paragraph::new(terminal_text(&format!(
            " DDB Debugger · {state} · {} · {} ",
            app.api_protocol,
            endpoint_label(&app.api_endpoint)
        )))
        .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_mini_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    frame.render_widget(
        Paragraph::new(terminal_text(&format!(" {} ", app.status)))
            .style(Style::default().fg(MUTED)),
        area,
    );
}

fn draw_toolbar(frame: &mut Frame<'_>, app: &App, area: Rect, areas: &mut UiAreas) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " DDB Debugger ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let controls = [
        (Control::Continue, "▶ Continue".to_string(), 11),
        (Control::Interrupt, "⏸ Pause".to_string(), 9),
        (Control::Next, "↷ Next".to_string(), 8),
        (Control::StepIn, "↓ Into".to_string(), 8),
        (Control::StepOut, "↑ Out".to_string(), 7),
        (Control::RevealFrame, "Frame F8".to_string(), 10),
        (Control::RefreshStack, "↻ DDB stack".to_string(), 12),
        (
            Control::CycleScope,
            format!("◎ {}", app.execution_scope_label()),
            22,
        ),
        (Control::Refresh, "⟳".to_string(), 3),
    ]
    .into_iter()
    .filter(|(control, _, _)| match control {
        Control::RefreshStack => app
            .capabilities
            .supports_ddb_feature("distributed_backtrace"),
        Control::CycleScope | Control::Refresh | Control::RevealFrame => true,
        control => control
            .action_name()
            .is_some_and(|action| app.capabilities.supports_execution(action)),
    });
    let status_width = 38.min(inner.width);
    let status_start = inner
        .x
        .saturating_add(inner.width.saturating_sub(status_width));
    let mut x = inner.x;
    for (control, label, width) in controls {
        if x.saturating_add(width) >= status_start {
            break;
        }
        let rect = Rect::new(x, inner.y, width, 1);
        let color = match control {
            Control::Continue => GREEN,
            Control::Interrupt => YELLOW,
            Control::RefreshStack | Control::CycleScope => ACCENT,
            _ => Color::White,
        };
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(color)),
            rect,
        );
        areas.controls.push((control, rect));
        x = x.saturating_add(width);
    }

    let connection = if app.api_connected {
        Span::styled("● connected", Style::default().fg(GREEN))
    } else {
        Span::styled("● reconnecting", Style::default().fg(RED))
    };
    let status_area = Rect::new(status_start, inner.y, status_width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            connection,
            Span::styled(
                format!(
                    "  {}  {}  events:{}  jobs:{}",
                    endpoint_label(&app.api_endpoint),
                    app.api_protocol,
                    if app.event_stream_connected {
                        "up"
                    } else {
                        "retry"
                    },
                    app.pending_commands
                ),
                Style::default().fg(MUTED),
            ),
        ]))
        .alignment(Alignment::Right),
        status_area,
    );
}

fn draw_compact_panel(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    let focus = if app.focus == Focus::Extensions
        && app.extension_panels.is_empty()
        && app.extension_actions.is_empty()
    {
        Focus::Timeline
    } else {
        app.focus
    };
    match focus {
        Focus::Threads => draw_threads(frame, app, area),
        Focus::Breakpoints => draw_breakpoints(frame, app, area, areas),
        Focus::Source => draw_source(frame, app, area, areas),
        Focus::Stack => draw_stack(frame, app, area),
        Focus::Variables => draw_variables(frame, app, area),
        Focus::Registers => draw_registers(frame, app, area),
        Focus::Memory => draw_memory(frame, app, area),
        Focus::Extensions => draw_extensions(frame, app, area, areas),
        Focus::Timeline => draw_timeline(frame, app, area),
        Focus::Output => draw_output(frame, app, area),
    }
    let (hit_area, first_item, item_height) = match focus {
        Focus::Threads => (
            area,
            list_first(
                app.selected_topology_row(),
                app.topology_rows().len(),
                area,
                1,
            ),
            1,
        ),
        Focus::Breakpoints => (area, 0, 0),
        Focus::Registers => (
            area,
            list_first(app.selected_register, app.registers.len(), area, 1),
            1,
        ),
        Focus::Memory => (
            area,
            list_first(app.selected_memory_row, app.memory_row_count(), area, 1),
            1,
        ),
        Focus::Source => (area, source_start(app), 1),
        Focus::Stack => (
            area,
            list_first(app.selected_frame, app.frames.len(), area, 1),
            1,
        ),
        Focus::Variables => {
            let variables_area = variable_list_area(app, area);
            (
                variables_area,
                list_first(
                    app.selected_variable,
                    app.variables.len(),
                    variables_area,
                    2,
                ),
                2,
            )
        }
        Focus::Extensions | Focus::Timeline | Focus::Output => (area, 0, 0),
    };
    if focus == Focus::Breakpoints {
        // Hierarchical breakpoint hit rows are registered by draw_breakpoints.
    } else if item_height == 0 {
        areas.add_focus_panel(focus, hit_area);
    } else {
        areas.add_panel(focus, hit_area, first_item, item_height);
        if focus == Focus::Variables && hit_area != area {
            register_hit_area(app, area, areas);
        }
    }
}

fn draw_threads(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let rows = app.topology_rows();
    let selected = app.selected_topology_row();
    let lines = rows
        .iter()
        .map(|row| {
            let (icon, label_color) = match row.kind {
                TopologyRowKind::Group(_) => ("▾", ACCENT),
                TopologyRowKind::Session(_) => ("◇", Color::White),
                TopologyRowKind::Thread(_) => ("•", Color::Gray),
            };
            let state_color = match row.state.as_deref() {
                Some("stopped") => YELLOW,
                Some("running") | Some("ready") => GREEN,
                Some("failed") => RED,
                _ => MUTED,
            };
            Line::from(vec![
                Span::raw("  ".repeat(row.depth)),
                Span::styled(format!("{icon} "), Style::default().fg(label_color)),
                Span::styled(terminal_text(&row.label), Style::default().fg(label_color)),
                Span::styled(
                    row.state
                        .as_ref()
                        .map(|state| format!("  {}", terminal_text(state)))
                        .unwrap_or_default(),
                    Style::default().fg(state_color),
                ),
                Span::styled(
                    if row.detail.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", terminal_text(&row.detail))
                    },
                    Style::default().fg(MUTED),
                ),
            ])
        })
        .collect::<Vec<_>>();
    app.topology_viewport.fit(
        lines.iter().map(Line::width).max().unwrap_or(0),
        area.width.saturating_sub(4) as usize,
    );
    let items = lines
        .into_iter()
        .map(|line| ListItem::new(horizontal_line(line, app.topology_viewport.column())))
        .collect();
    draw_selectable_list(
        frame,
        area,
        title(app, Focus::Threads, "DDB Topology"),
        items,
        selected,
        app.focus == Focus::Threads,
    );
}

fn draw_breakpoints(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    let mut rows = Vec::new();
    let hits = app.breakpoint_hits();
    for (index, breakpoint) in app.breakpoints.iter().enumerate() {
        let current_hits = hits
            .iter()
            .filter(|hit| hit.breakpoint_id == breakpoint.id)
            .collect::<Vec<_>>();
        let mut attributes = Vec::new();
        if breakpoint.temporary {
            attributes.push("temporary".to_string());
        }
        if breakpoint.hardware {
            attributes.push("hardware".to_string());
        }
        if breakpoint.pending {
            attributes.push("pending".to_string());
        } else if !breakpoint.verified {
            attributes.push("unverified".to_string());
        }
        if let Some(condition) = breakpoint.condition.as_ref() {
            attributes.push(format!("if {}", terminal_text(condition)));
        }
        if let Some(ignore_count) = breakpoint.ignore_count {
            attributes.push(format!("ignore next {ignore_count}"));
        }
        let attributes = if attributes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", attributes.join(", "))
        };
        let scope = breakpoint_target_label(app, breakpoint.target.as_ref());
        rows.push((
            index,
            None,
            Line::from(vec![
                Span::styled(
                    if current_hits.is_empty() {
                        String::new()
                    } else {
                        format!("▶ {} paused · ", current_hits.len())
                    },
                    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if breakpoint.enabled { "● " } else { "○ " },
                    Style::default().fg(if breakpoint.enabled { RED } else { MUTED }),
                ),
                Span::styled(
                    format!(
                        "{}:{} ",
                        terminal_text(short_path(&breakpoint.location.src)),
                        breakpoint.location.line,
                    ),
                    Style::default().fg(ACCENT),
                ),
                Span::raw(format!("hits:{}{}", breakpoint.times, attributes)),
                Span::styled(
                    format!(" · {scope} · {} sites", breakpoint.sub_breakpoints.len()),
                    Style::default().fg(YELLOW),
                ),
                Span::styled(
                    breakpoint
                        .message
                        .as_ref()
                        .map(|message| format!(" · {}", terminal_text(message)))
                        .unwrap_or_default(),
                    Style::default().fg(MUTED),
                ),
            ]),
        ));
        for (site, member) in breakpoint.sub_breakpoints.iter().enumerate() {
            let site_hits = current_hits
                .iter()
                .filter(|hit| hit.matches_member(breakpoint, member))
                .count();
            let session = session_display_name(app, &member.session_id);
            let location = member
                .location
                .as_ref()
                .map(|location| {
                    format!(
                        "{}:{}",
                        location
                            .path
                            .as_deref()
                            .map(short_path)
                            .unwrap_or("<pending>"),
                        location.line
                    )
                })
                .unwrap_or_else(|| "<pending>".to_string());
            let message = member
                .message
                .as_ref()
                .map(|message| format!(" · {}", terminal_text(message)))
                .unwrap_or_default();
            rows.push((
                index,
                Some(site),
                Line::from(vec![
                    Span::styled(
                        if site_hits == 0 {
                            String::new()
                        } else {
                            format!("▶ {site_hits} paused · ")
                        },
                        Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  ↳ ", Style::default().fg(MUTED)),
                    Span::styled(session, Style::default().fg(Color::White)),
                    Span::styled(
                        format!(
                            " · {location} · {} · hits:{}{}",
                            if member.verified {
                                "verified"
                            } else {
                                "unverified"
                            },
                            member.hit_count,
                            message,
                        ),
                        Style::default().fg(if member.verified { GREEN } else { YELLOW }),
                    ),
                ]),
            ));
        }
    }
    let selected = rows
        .iter()
        .position(|(index, site, _)| {
            *index == app.selected_breakpoint && *site == app.selected_breakpoint_site
        })
        .unwrap_or(0);
    let first = list_first(selected, rows.len(), area, 1);
    app.breakpoint_viewport.fit(
        rows.iter()
            .map(|(_, _, line)| line.width())
            .max()
            .unwrap_or(0),
        area.width.saturating_sub(4) as usize,
    );
    let parents = rows
        .iter()
        .map(|(index, site, _)| (*index, *site))
        .collect::<Vec<_>>();
    let items = rows
        .into_iter()
        .map(|(_, _, line)| ListItem::new(horizontal_line(line, app.breakpoint_viewport.column())))
        .collect::<Vec<_>>();
    draw_selectable_list(
        frame,
        area,
        title(app, Focus::Breakpoints, "DDB Breakpoints"),
        items,
        selected,
        app.focus == Focus::Breakpoints,
    );
    areas.add_focus_panel(Focus::Breakpoints, area);
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let end = (first + inner.height as usize).min(parents.len());
    for (row, (parent, site)) in parents[first..end].iter().enumerate() {
        areas.breakpoint_sites.push((
            *parent,
            *site,
            Rect::new(inner.x, inner.y.saturating_add(row as u16), inner.width, 1),
        ));
        areas.breakpoint_rows.push((
            *parent,
            Rect::new(inner.x, inner.y.saturating_add(row as u16), inner.width, 1),
        ));
    }
}

fn session_display_name(app: &App, session_id: &str) -> String {
    app.sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .map(|session| {
            if session.display_name.is_empty() {
                session.session_id.clone()
            } else {
                session.display_name.clone()
            }
        })
        .unwrap_or_else(|| session_id.to_string())
}

fn breakpoint_target_label(app: &App, value: Option<&v2::Target>) -> String {
    match value.and_then(|value| value.selector.as_ref()) {
        Some(target::Selector::Session(value)) => {
            format!("session {}", session_display_name(app, &value.session_id))
        }
        Some(target::Selector::Thread(value)) => format!("thread {}", value.thread_id),
        Some(target::Selector::Group(value)) => {
            let name = app
                .groups
                .iter()
                .find(|group| group.group_id == value.group_id)
                .map(|group| {
                    if group.display_name.is_empty() {
                        group.group_id.as_str()
                    } else {
                        group.display_name.as_str()
                    }
                })
                .unwrap_or(value.group_id.as_str());
            format!("group {name}")
        }
        Some(target::Selector::SessionSet(value)) => {
            format!("set of {} sessions", value.session_ids.len())
        }
        Some(target::Selector::Broadcast(_)) => "all sessions".to_string(),
        Some(target::Selector::Multiple(value)) => value
            .targets
            .iter()
            .map(|target| breakpoint_target_label(app, Some(target)))
            .collect::<Vec<_>>()
            .join(" + "),
        Some(target::Selector::CurrentThread(_)) => "current thread".to_string(),
        Some(target::Selector::CurrentSession(_)) => "current session".to_string(),
        Some(target::Selector::First(_)) => "first session".to_string(),
        Some(target::Selector::Operation(_)) => "operation".to_string(),
        None => "unknown scope".to_string(),
    }
}

fn draw_source(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    let source_label = app
        .source_browser
        .document()
        .map(|source| {
            let source_owners = source.source.owners(&app.sessions);
            let owner_names = source_owners
                .iter()
                .take(3)
                .map(|owner| {
                    app.sessions
                        .iter()
                        .find(|session| session.session_id == owner.session_id)
                        .map(|session| session.display_name.as_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&owner.session_id)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let owners = if owner_names.is_empty() {
                "unbound".to_owned()
            } else if source_owners.len() > 3 {
                format!("{owner_names}, +{}", source_owners.len() - 3)
            } else {
                owner_names
            };
            format!(
                "[{}] {} · {} · {} · {} lines",
                terminal_text(&owners),
                terminal_text(&source.path),
                if source.source.is_local() {
                    "Local"
                } else {
                    "DDB"
                },
                if app.source_browser.following() {
                    "following"
                } else {
                    "browsing · f: follow"
                },
                source.total_lines
            )
        })
        .unwrap_or_else(|| "Source".to_string());
    let loading = app.source_browser.pending_generation().is_some();
    let source_label = if loading {
        format!("Loading source · {source_label}")
    } else {
        source_label
    };
    let source_title = title(app, Focus::Source, &source_label);
    let block = panel_block(&source_title, app.focus == Focus::Source);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if area.width >= 12 && area.height > 0 {
        let open = Rect::new(area.right() - 11, area.y, 10, 1);
        frame.render_widget(
            Paragraph::new(" Open [o] ").style(Style::default().fg(ACCENT)),
            open,
        );
        areas.source_explorer_open = Some(open);
    }
    app.source_browser.fit_viewport(inner.height as usize);
    app.source_browser
        .fit_horizontal(inner.width.saturating_sub(12) as usize);
    if let Some(error) = app.source_browser.error() {
        frame.render_widget(Paragraph::new(format!(
            "Source unavailable\n\n{}\n\nF8: retry selected frame | o: browse source roots | Ctrl-P: debugger files",
            terminal_text(error)
        )).wrap(Wrap { trim: false }).style(Style::default().fg(RED)), inner);
        return;
    }
    let Some(source) = app.source_browser.document() else {
        frame.render_widget(
            Paragraph::new(if loading {
                "Resolving or reading source…\n\nYou can use o, Ctrl-P, or g to open another file."
            } else {
                "o: source roots · Ctrl-P: debugger files · g: path:line"
            })
            .style(Style::default().fg(MUTED))
            .alignment(Alignment::Center),
            inner,
        );
        return;
    };

    let height = inner.height as usize;
    let start = source_start(app);
    let binding = source.debugger_binding(&app.sessions);
    let end = (start + height).min(source.lines.len());
    let lines = source.lines[start..end]
        .iter()
        .enumerate()
        .map(|(offset, text)| {
            let line_number = source.start_line + start + offset;
            let is_executing = app.execution_location.as_ref().is_some_and(|location| {
                location.line == line_number
                    && binding.as_ref().is_some_and(|binding| {
                        binding.matches_location(&location.session_id, &location.source)
                    })
            });
            let is_distributed_stop = !is_executing
                && app.threads.iter().any(|thread| {
                    thread.state.eq_ignore_ascii_case("stopped")
                        && thread.line == Some(line_number as u64)
                        && thread.file.as_ref().is_some_and(|path| {
                            binding.as_ref().is_some_and(|binding| {
                                binding.matches_location(&thread.session_id, path)
                            })
                        })
                });
            let is_cursor = source.cursor == line_number;
            let has_breakpoint = app.breakpoints.iter().any(|breakpoint| {
                binding.as_ref().is_some_and(|binding| {
                    binding.marks_breakpoint(breakpoint, line_number, &app.sessions)
                })
            });
            let style = if is_executing {
                Style::default()
                    .fg(Color::Black)
                    .bg(YELLOW)
                    .add_modifier(Modifier::BOLD)
            } else if is_distributed_stop {
                Style::default().fg(Color::White).bg(Color::Rgb(32, 72, 68))
            } else if is_cursor {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(38, 79, 120))
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    if is_executing {
                        "▶"
                    } else if is_distributed_stop {
                        "◆"
                    } else {
                        " "
                    },
                    Style::default().fg(YELLOW),
                ),
                Span::styled(
                    if has_breakpoint { "●" } else { " " },
                    Style::default().fg(RED),
                ),
                Span::styled(
                    if is_cursor { "▸" } else { " " },
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    format!(" {:>5} │ ", line_number),
                    Style::default().fg(MUTED),
                ),
                Span::raw(horizontal_text(text, source.column)),
            ])
            .style(style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);

    if source.lines.len() > height {
        let mut scrollbar = ScrollbarState::new(source.lines.len()).position(start);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar,
        );
    }
}

fn draw_stack(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let lines = app
        .frames
        .iter()
        .map(|stack_frame| {
            let session = app
                .sessions
                .iter()
                .find(|session| session.session_id == stack_frame.session_id)
                .map(|session| session.display_name.as_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(stack_frame.session_id.as_str());
            if stack_frame.boundary {
                return Line::from(vec![
                    Span::styled("── ", Style::default().fg(YELLOW)),
                    Span::styled(
                        terminal_text(&stack_frame.function),
                        Style::default().fg(YELLOW),
                    ),
                    Span::styled(
                        format!(
                            " → {} · {} ──",
                            terminal_text(session),
                            terminal_text(&stack_frame.thread_id)
                        ),
                        Style::default().fg(MUTED),
                    ),
                ]);
            }
            Line::from(vec![
                Span::styled(
                    format!("#{:<3}", stack_frame.distributed_index),
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    terminal_text(&stack_frame.function),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(
                        "  [{} · local #{}]  {}:{}  {}",
                        terminal_text(session),
                        stack_frame.level,
                        terminal_text(stack_frame.file.as_deref().map(short_path).unwrap_or("?"),),
                        stack_frame.line.unwrap_or(0),
                        terminal_text(&stack_frame.address)
                    ),
                    Style::default().fg(MUTED),
                ),
            ])
        })
        .collect::<Vec<_>>();
    app.stack_viewport.fit(
        lines.iter().map(Line::width).max().unwrap_or(0),
        area.width.saturating_sub(4) as usize,
    );
    let items = lines
        .into_iter()
        .map(|line| ListItem::new(horizontal_line(line, app.stack_viewport.column())))
        .collect();
    let stack_title = if app.distributed_stack_truncation.is_some() {
        "DDB Distributed Call Stack · TRUNCATED"
    } else {
        "DDB Distributed Call Stack"
    };
    draw_selectable_list(
        frame,
        area,
        title(app, Focus::Stack, stack_title),
        items,
        app.selected_frame,
        app.focus == Focus::Stack,
    );
}

fn draw_variables(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let (variables_area, registers_area, memory_area) = inspection_areas(app, area);
    let rows = app
        .variables
        .iter()
        .map(|variable| {
            let indent = "  ".repeat(variable.depth);
            let disclosure = if variable.loading {
                "… "
            } else if variable.has_children {
                if variable.expanded {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "  "
            };
            vec![
                Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(disclosure, Style::default().fg(YELLOW)),
                    Span::styled(terminal_text(&variable.name), Style::default().fg(ACCENT)),
                    Span::styled(
                        format!(" : {}", terminal_text(&variable.type_name)),
                        Style::default().fg(MUTED),
                    ),
                ]),
                Line::from(vec![
                    Span::raw(format!("{indent}    ")),
                    Span::styled(terminal_text(&variable.value), Style::default().fg(GREEN)),
                    Span::styled(
                        if variable.has_children {
                            if variable.children > 0 {
                                format!("  [{} children]", variable.children)
                            } else {
                                "  [children]".to_string()
                            }
                        } else {
                            String::new()
                        },
                        Style::default().fg(MUTED),
                    ),
                ]),
            ]
        })
        .collect::<Vec<_>>();
    app.variable_viewport.fit(
        rows.iter().flatten().map(Line::width).max().unwrap_or(0),
        variables_area.width.saturating_sub(4) as usize,
    );
    let items = rows
        .into_iter()
        .map(|lines| {
            ListItem::new(
                lines
                    .into_iter()
                    .map(|line| horizontal_line(line, app.variable_viewport.column()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    draw_selectable_list(
        frame,
        variables_area,
        title(
            app,
            Focus::Variables,
            "Variables / Watches · w add · Enter expand",
        ),
        items,
        app.selected_variable,
        app.focus == Focus::Variables,
    );
    if let Some(registers_area) = registers_area {
        draw_registers(frame, app, registers_area);
    }
    if let Some(memory_area) = memory_area {
        draw_memory(frame, app, memory_area);
    }
}

fn inspection_areas(app: &App, area: Rect) -> (Rect, Option<Rect>, Option<Rect>) {
    match (app.registers.is_empty(), app.memory.is_empty()) {
        (true, true) => (area, None, None),
        (false, true) => {
            let areas = Layout::vertical([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(area);
            (areas[0], Some(areas[1]), None)
        }
        (true, false) => {
            let areas = Layout::vertical([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(area);
            (areas[0], None, Some(areas[1]))
        }
        (false, false) => {
            let areas = Layout::vertical([
                Constraint::Percentage(45),
                Constraint::Percentage(25),
                Constraint::Percentage(30),
            ])
            .split(area);
            (areas[0], Some(areas[1]), Some(areas[2]))
        }
    }
}

fn draw_registers(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let lines = app
        .registers
        .iter()
        .map(|register| {
            Line::from(vec![
                Span::styled(
                    format!("{} ", terminal_text(&register.name)),
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    terminal_text(&register.value),
                    Style::default().fg(if register.unavailable { MUTED } else { GREEN }),
                ),
            ])
        })
        .collect::<Vec<_>>();
    app.register_viewport.fit(
        lines.iter().map(Line::width).max().unwrap_or(0),
        usize::from(area.width.saturating_sub(4)),
    );
    let lines = lines
        .into_iter()
        .map(|line| ListItem::new(horizontal_line(line, app.register_viewport.column())))
        .collect();
    draw_selectable_list(
        frame,
        area,
        "Registers · = edit".into(),
        lines,
        app.selected_register,
        app.focus == Focus::Registers,
    );
}

fn register_hit_area(app: &App, area: Rect, areas: &mut UiAreas) {
    let (_, registers, memory) = inspection_areas(app, area);
    if let Some(area) = registers {
        areas.add_panel(
            Focus::Registers,
            area,
            list_first(app.selected_register, app.registers.len(), area, 1),
            1,
        );
    }
    if let Some(area) = memory {
        areas.add_panel(
            Focus::Memory,
            area,
            list_first(app.selected_memory_row, app.memory_row_count(), area, 1),
            1,
        );
    }
}

fn variable_list_area(app: &App, area: Rect) -> Rect {
    inspection_areas(app, area).0
}

fn draw_memory(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let byte_count = app
        .memory
        .iter()
        .map(|block| block.bytes.len())
        .sum::<usize>();
    let memory_title = format!("Raw Memory · {byte_count} B");
    let mut lines = Vec::new();
    for memory in &app.memory {
        let base = u64::from_str_radix(memory.begin.trim_start_matches("0x"), 16).ok();
        for (row, bytes) in memory.bytes.chunks(16).enumerate() {
            let address = base
                .and_then(|base| base.checked_add((row * 16) as u64))
                .map(|address| format!("{address:016x}"))
                .unwrap_or_else(|| format!("{}+{:x}", terminal_text(&memory.begin), row * 16));
            let hex = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = bytes
                .iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        char::from(*byte)
                    } else {
                        '.'
                    }
                })
                .collect::<String>();
            lines.push(Line::from(vec![
                Span::styled(address, Style::default().fg(ACCENT)),
                Span::raw(format!("  {hex:<47}  ")),
                Span::styled(format!("|{ascii}|"), Style::default().fg(GREEN)),
            ]));
        }
    }
    app.memory_viewport.fit(
        lines.iter().map(Line::width).max().unwrap_or(0),
        area.width.saturating_sub(4) as usize,
    );
    let items = lines
        .into_iter()
        .map(|line| ListItem::new(horizontal_line(line, app.memory_viewport.column())))
        .collect();
    draw_selectable_list(
        frame,
        area,
        memory_title,
        items,
        app.selected_memory_row,
        app.focus == Focus::Memory,
    );
}

fn draw_output(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let mode = if app.output.following() {
        "live"
    } else {
        "history"
    };
    let label = format!(
        "Output · {mode} · O hide · End follow · {} lines evicted",
        app.output.dropped
    );
    let block = panel_block(&label, app.focus == Focus::Output);
    let inner = block.inner(area);
    app.output.fit(inner.width as usize, inner.height as usize);
    let lines = app
        .output
        .visible_lines()
        .map(|line| Line::from(horizontal_text(line, app.output.column())))
        .collect::<Vec<_>>();
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_timeline(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let timeline_title = title(app, Focus::Timeline, "DDB Timeline");
    let block = panel_block(&timeline_title, app.focus == Focus::Timeline);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (start, end) = timeline_window(app, area);
    let lines = app
        .timeline
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|line| {
            Line::from(Span::styled(
                terminal_text(line),
                Style::default().fg(Color::Gray),
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_extensions(frame: &mut Frame<'_>, app: &App, area: Rect, areas: &mut UiAreas) {
    let extension_title = title(app, Focus::Extensions, "Extensions");
    let block = panel_block(&extension_title, app.focus == Focus::Extensions);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let presentation_area = if app.extension_actions.is_empty() {
        inner
    } else {
        let rows = if app.extension_panels.is_empty() {
            Layout::vertical([Constraint::Min(1), Constraint::Length(0)]).split(inner)
        } else {
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner)
        };
        draw_extension_actions(frame, app, rows[0], areas);
        if app.extension_panels.is_empty() {
            return;
        }
        rows[1]
    };
    let inner = presentation_area;
    let mut lines = Vec::new();
    for panel in &app.extension_panels {
        lines.push(Line::from(vec![
            Span::styled(
                terminal_text(&panel.extension_title),
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                format!(" · {}", terminal_text(&panel.panel_title)),
                Style::default().fg(YELLOW),
            ),
        ]));
        if !panel.description.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", terminal_text(&panel.description)),
                Style::default().fg(MUTED),
            )));
        }
        if panel.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no data",
                Style::default().fg(MUTED),
            )));
        } else {
            for row in &panel.rows {
                let cells = row
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let column = panel
                            .columns
                            .get(index)
                            .map(String::as_str)
                            .unwrap_or("Value");
                        format!("{}: {}", terminal_text(column), terminal_text(value))
                    })
                    .collect::<Vec<_>>()
                    .join("  ");
                lines.push(Line::from(format!("  {cells}")));
            }
        }
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let visual_lines = paragraph.line_count(inner.width);
    let start = app
        .extension_scroll
        .min(visual_lines.saturating_sub(inner.height as usize));
    let vertical_scroll = u16::try_from(start).unwrap_or(u16::MAX);
    frame.render_widget(paragraph.scroll((vertical_scroll, 0)), inner);
}

fn draw_extension_actions(frame: &mut Frame<'_>, app: &App, area: Rect, areas: &mut UiAreas) {
    let Some(action) = app.extension_actions.get(app.selected_extension_action) else {
        return;
    };
    let position = format!(
        "Action {}/{} · ",
        app.selected_extension_action + 1,
        app.extension_actions.len()
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(position, Style::default().fg(YELLOW)),
            Span::styled(
                format!(
                    "{} · {}",
                    terminal_text(&action.extension_title),
                    terminal_text(&action.title)
                ),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  [a/Enter/click]", Style::default().fg(MUTED)),
        ])),
        area,
    );
    areas
        .extension_action_rows
        .push((app.selected_extension_action, area));
}

fn draw_prompt(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (title, prefix, color) = match app.input_mode {
        InputMode::Command => ("Command", ":", ACCENT),
        InputMode::Evaluate => ("Evaluate", "e> ", GREEN),
        InputMode::Watch => ("Add watch expression", "w> ", GREEN),
        InputMode::AssignValue => ("Assign selected value (Esc cancels)", "= ", YELLOW),
        InputMode::Memory => ("Read memory", "m> ", YELLOW),
        InputMode::Jump => ("Jump execution location", "J> ", YELLOW),
        InputMode::Signal => ("Send signal", "s> ", RED),
        InputMode::GotoLine => ("Open source (line or path:line)", "g> ", ACCENT),
        InputMode::Breakpoint => (
            "Breakpoint (-t -h --ignore-count N if CONDITION)",
            "B> ",
            RED,
        ),
        InputMode::FunctionBreakpoint => ("Function breakpoint (symbol [ -- OPTIONS])", "F> ", RED),
        InputMode::ExtensionAction => ("Extension action JSON", "a> ", ACCENT),
        InputMode::Normal => ("Status", "", MUTED),
    };
    let content = if app.input_mode == InputMode::Normal {
        Line::from(terminal_text(&app.status))
    } else {
        input_line(prefix, &app.input, app.input_cursor, color)
    };
    frame.render_widget(
        Paragraph::new(content)
            .style(Style::default().fg(color))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} ")),
            ),
        area,
    );
}

fn input_line(prefix: &str, input: &str, cursor: usize, color: Color) -> Line<'static> {
    let before = input.chars().take(cursor).collect::<String>();
    let current = input
        .chars()
        .nth(cursor)
        .map(|character| character.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after = input.chars().skip(cursor + 1).collect::<String>();
    Line::from(vec![
        Span::styled(prefix.to_string(), Style::default().fg(color)),
        Span::styled(before, Style::default().fg(color)),
        Span::styled(current, Style::default().fg(Color::Black).bg(color)),
        Span::styled(after, Style::default().fg(color)),
    ])
}

fn draw_hints(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(" Tab panels  ↑↓ navigate  b/B breakpoint  c execution scope  d refresh DDB stack  e eval  ? help ")
            .style(Style::default().fg(MUTED))
            .alignment(Alignment::Center),
        area,
    );
}

fn draw_signal_picker(frame: &mut Frame<'_>, app: &App, root: Rect, areas: &mut UiAreas) {
    let Some(picker) = app.signal_picker.as_ref() else {
        return;
    };
    let area = centered(root, 76, 70);
    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " DDB signal catalog ",
        Style::default().fg(RED).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let first = list_first(picker.cursor, picker.signals.len(), rows[0], 1);
    let items = picker
        .signals
        .iter()
        .skip(first)
        .take(rows[0].height as usize)
        .map(|signal| {
            let disposition = format!(
                "  stop:{} print:{} pass:{}",
                if signal.stop { "yes" } else { "no" },
                if signal.print { "yes" } else { "no" },
                if signal.pass { "yes" } else { "no" },
            );
            ListItem::new(Line::from(vec![
                Span::styled(terminal_text(&signal.name), Style::default().fg(YELLOW)),
                Span::styled(disposition, Style::default().fg(MUTED)),
                Span::styled(
                    signal
                        .description
                        .as_ref()
                        .map(|description| format!("  {}", terminal_text(description)))
                        .unwrap_or_default(),
                    Style::default().fg(Color::Gray),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let visible = items.len();
    let mut state = ListState::default();
    if visible > 0 {
        state.select(Some(picker.cursor.saturating_sub(first).min(visible - 1)));
    }
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(92, 45, 45))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, rows[0], &mut state);
    areas.signal_rows.extend((0..visible).map(|offset| {
        (
            first + offset,
            Rect {
                x: rows[0].x,
                y: rows[0].y.saturating_add(offset as u16),
                width: rows[0].width,
                height: 1,
            },
        )
    }));

    frame.render_widget(
        Paragraph::new(" Enter/click send · f custom signal · Esc cancel")
            .style(Style::default().fg(MUTED)),
        rows[1],
    );
    let cancel_width = 11.min(rows[1].width);
    areas.signal_cancel = Some(Rect::new(
        rows[1].right().saturating_sub(cancel_width),
        rows[1].y,
        cancel_width,
        1,
    ));
}

fn draw_breakpoint_target_picker(
    frame: &mut Frame<'_>,
    app: &App,
    root: Rect,
    areas: &mut UiAreas,
) {
    let Some(picker) = app.breakpoint_target_picker.as_ref() else {
        return;
    };
    let area = centered(root, 78, 72);
    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " DDB breakpoint targets ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let visible = rows[0].height.max(1) as usize;
    let first = picker
        .cursor
        .saturating_add(1)
        .saturating_sub(visible)
        .min(picker.choices.len().saturating_sub(visible));
    let items = picker
        .choices
        .iter()
        .map(|choice| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    if choice.selected { "[×] " } else { "[ ] " },
                    Style::default().fg(if choice.selected { GREEN } else { MUTED }),
                ),
                Span::styled(
                    terminal_text(&choice.label),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("  {}", terminal_text(&choice.detail)),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(picker.cursor.min(items.len() - 1)));
        *state.offset_mut() = first;
    }
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .bg(Color::Rgb(36, 45, 58))
                .add_modifier(Modifier::BOLD),
        ),
        rows[0],
        &mut state,
    );
    let end = (first + visible).min(picker.choices.len());
    for (row, index) in (first..end).enumerate() {
        areas.breakpoint_target_rows.push((
            index,
            Rect::new(
                rows[0].x,
                rows[0].y.saturating_add(row as u16),
                rows[0].width,
                1,
            ),
        ));
    }

    let actions = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ])
    .split(rows[1]);
    frame.render_widget(
        Paragraph::new("Space toggle · ↑↓ move").style(Style::default().fg(MUTED)),
        actions[0],
    );
    frame.render_widget(
        Paragraph::new("[ Apply ]")
            .alignment(Alignment::Center)
            .style(Style::default().fg(GREEN).add_modifier(Modifier::BOLD)),
        actions[1],
    );
    frame.render_widget(
        Paragraph::new("[ Cancel ]")
            .alignment(Alignment::Center)
            .style(Style::default().fg(YELLOW)),
        actions[2],
    );
    areas.breakpoint_target_apply = Some(actions[1]);
    areas.breakpoint_target_cancel = Some(actions[2]);
}
fn draw_help(frame: &mut Frame<'_>, app: &mut App, root: Rect) {
    let area = centered(root, 96, 94);
    frame.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(" Help ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(inner);
    let text = vec![
        Line::from(Span::styled(
            "DDB TUI shortcuts",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled("Execution", Style::default().fg(ACCENT))),
        Line::from("F5 continue     F6 pause       F10 next"),
        Line::from("F11 step in     Shift+F11 out"),
        Line::from("F8 reveal selected stack frame in Source"),
        Line::from("c cycle target: thread / session / group / all"),
        Line::from("s choose signal; J opens the execution-jump prompt"),
        Line::from(""),
        Line::from(Span::styled(
            "Source and breakpoints",
            Style::default().fg(ACCENT),
        )),
        Line::from("source gutter: ▶ execution  ▸ cursor  ● breakpoint"),
        Line::from("b toggle breakpoint at source cursor"),
        Line::from("B conditional/temporary/hardware/ignore-count breakpoint"),
        Line::from("Breakpoints: P/right-click picks a paused hit; Enter/click opens source"),
        Line::from("F function breakpoint, then choose session/group targets"),
        Line::from("Delete remove selected breakpoint"),
        Line::from("x / Space enable or disable selected breakpoint"),
        Line::from(""),
        Line::from(Span::styled(
            "Inspection and navigation",
            Style::default().fg(ACCENT),
        )),
        Line::from("d refresh DDB distributed stack   e evaluate expression"),
        Line::from("Variables / Registers: = assign the selected value; Esc cancels"),
        Line::from("m memory: ADDRESS or ADDRESS ; BYTE_COUNT"),
        Line::from("memory: Up/Down or wheel browse rows; Left/Right scroll columns; Home resets; z expand"),
        Line::from("g open source: line or path:line   J jump execution   s signal"),
        Line::from("j/k or Up/Down navigate; Page Up/Down moves one visible page"),
        Line::from("source: f follow execution   Alt+Left/Right navigation history"),
        Line::from("source: Left/Right scroll horizontally; Home returns to left edge"),
        Line::from("w add watch; Variables: Delete removes a watch; Enter expands"),
        Line::from("o source explorer · Ctrl-P debugger files · type to search · Enter/click open"),
        Line::from("extensions: arrows select; a/Enter/click invokes declared action"),
        Line::from("prompt: arrows edit/history, Home/End, paste supported"),
        Line::from(": native debugger command in selected frame   r refresh"),
        Line::from("Tab / Shift+Tab move focus      arrows / wheel navigate"),
        Line::from("z expand/restore pane   Esc restore   t toggle timeline"),
        Line::from("O output pane; Up/Down/Page scroll; Home oldest; End follow; Left/Right"),
        Line::from("registers: Up/Down browse; Left/Right or horizontal wheel scroll; Home resets; z expand"),
        Line::from("Topology/Breakpoints/Stack/Variables: Left/Right scroll; Home resets"),
        Line::from("click a panel or toolbar control to activate it"),
        Line::from(""),
        Line::from(Span::styled(
            "Press ? or Esc to close",
            Style::default().fg(YELLOW),
        )),
    ];
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    app.help_page_height = usize::from(rows[0].height).max(1);
    app.help_max_scroll = paragraph
        .line_count(rows[0].width.max(1))
        .saturating_sub(app.help_page_height);
    app.help_scroll = app.help_scroll.min(app.help_max_scroll);
    frame.render_widget(
        paragraph.scroll((app.help_scroll.min(u16::MAX as usize) as u16, 0)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(format!(
            "Up/Down, PgUp/PgDn, wheel | {}/{}\nEsc or ? closes help",
            app.help_scroll + 1,
            app.help_max_scroll + 1
        ))
        .style(Style::default().fg(YELLOW)),
        rows[1],
    );
}

fn draw_selectable_list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: String,
    items: Vec<ListItem<'_>>,
    selected: usize,
    focused: bool,
) {
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(panel_block(&title, focused))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(38, 79, 120))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn panel_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { ACCENT } else { Color::DarkGray }))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if focused { ACCENT } else { Color::Gray })
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ))
}

fn title(app: &App, focus: Focus, label: &str) -> String {
    if app.focus == focus {
        format!("◆ {label}")
    } else {
        label.to_string()
    }
}

fn short_path(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn source_start(app: &App) -> usize {
    app.source_browser
        .document()
        .map(|source| source.scroll.min(source.lines.len().saturating_sub(1)))
        .unwrap_or_default()
}

fn timeline_window(app: &App, area: Rect) -> (usize, usize) {
    if app.timeline.is_empty() {
        return (0, 0);
    }
    let width = area.width.saturating_sub(2).max(1) as usize;
    let mut remaining_rows = area.height.saturating_sub(2).max(1) as usize;
    let end = app.timeline_scroll.min(app.timeline.len() - 1) + 1;
    let mut start = end;
    for index in (0..end).rev() {
        let characters = app.timeline[index].chars().count().max(1);
        let rows = characters.saturating_add(width - 1) / width;
        if rows > remaining_rows && start < end {
            break;
        }
        start = index;
        remaining_rows = remaining_rows.saturating_sub(rows);
        if remaining_rows == 0 {
            break;
        }
    }
    (start, end)
}

fn list_first(selected: usize, len: usize, area: Rect, item_height: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let visible = (area.height.saturating_sub(2) as usize / item_height.max(1)).max(1);
    selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(len.saturating_sub(visible))
}

fn endpoint_label(endpoint: &str) -> &str {
    endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint)
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

#[cfg(test)]
mod v2_tests {
    use ddb_api_client::v2;
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::api::BreakpointTarget;
    use crate::model::{
        Breakpoint, BreakpointDraft, BreakpointLocation, BreakpointOptions, BreakpointTargetChoice,
        BreakpointTargetPicker,
    };

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.areas = draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn typed_signal_catalog_is_visible_and_mouse_addressable() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.open_signal_picker(
            crate::api::session_target("session/alpha"),
            vec![v2::DebuggerSignal {
                name: "SIGINT".to_string(),
                stop: true,
                print: true,
                pass: false,
                description: Some("Interrupt".to_string()),
            }],
        )
        .unwrap();
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("DDB signal catalog"));
        assert!(screen.contains("SIGINT"));
        assert!(screen.contains("stop:yes print:yes pass:no"));
        let (_, area) = app.areas.signal_rows[0];
        assert_eq!(app.areas.signal_at(area.x, area.y), Some(0),);
        assert!(app.areas.signal_cancel.is_some());
    }

    #[test]
    fn default_runtime_has_no_framework_specific_ui() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_capabilities(v2::Capabilities::default());
        app.apply_snapshot(v2::Snapshot::default());
        let screen = render(&mut app, 140, 40);
        assert!(!screen.to_lowercase().contains("proclet"));
        assert!(!screen.contains("Extensions"));
        assert!(!screen.contains("DDB Timeline"));
    }

    #[test]
    fn output_pane_reveals_multiline_and_long_output_without_timeline() {
        let mut app = App::new("http://localhost".into());
        app.show_output = true;
        app.output.push("output first line\noutput second line");
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("output first line") && screen.contains("output second line"));
        assert!(!screen.contains("DDB Timeline"));
        assert!(app
            .areas
            .panels
            .iter()
            .any(|pane| pane.focus == Focus::Output));
        app.focus = Focus::Output;
        app.expanded_pane = true;
        app.output.push(&format!("{}OUTPUT_END", "界".repeat(100)));
        assert!(!render(&mut app, 45, 14).contains("OUTPUT_END"));
        app.output.scroll_horizontal(isize::MAX);
        assert!(render(&mut app, 45, 14).contains("OUTPUT_END"));
    }

    #[test]
    fn horizontal_inspection_scrolling_reveals_clipped_text_and_clamps_on_resize() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Variables;
        app.expanded_pane = true;
        app.apply_variables(vec![v2::Variable {
            variable_id: "long/value".into(),
            name: "message".into(),
            value: format!("{}VALUE_END", "界".repeat(50)),
            ..Default::default()
        }]);
        assert!(!render(&mut app, 45, 14).contains("VALUE_END"));
        app.variable_viewport.scroll(isize::MAX);
        assert!(render(&mut app, 45, 14).contains("VALUE_END"));
        assert_eq!(app.selected_variable, 0);
        render(&mut app, 200, 40);
        assert_eq!(app.variable_viewport.column(), 0);
        app.frames.push(crate::model::StackFrame {
            id: "frame/long".into(),
            distributed_index: 0,
            session_id: String::new(),
            thread_id: "thread/long".into(),
            level: 0,
            address: "STACK_END".into(),
            function: "long_function_".repeat(10),
            file: None,
            line: None,
            boundary: false,
        });
        app.focus = Focus::Stack;
        assert!(!render(&mut app, 45, 14).contains("STACK_END"));
        app.stack_viewport.scroll(isize::MAX);
        assert!(render(&mut app, 45, 14).contains("STACK_END"));
        assert_eq!(app.selected_frame, 0);
        app.frames.clear();
        render(&mut app, 45, 14);
        assert_eq!(app.stack_viewport.column(), 0);
    }

    #[test]
    fn pane_expansion_buttons_are_visible_on_borders_and_fit_small_terminals() {
        let mut app = App::new("http://localhost".into());
        assert!(render(&mut app, 140, 40).contains("[Expand]"));
        for (focus, button) in &app.areas.pane_expanders {
            let pane = app.areas.panels.iter().find(|p| p.focus == *focus).unwrap();
            assert_eq!(button.y, pane.area.bottom() - 1);
            assert!(button.right() < pane.area.right());
            assert!(app.areas.item_at(button.x, button.y).is_none());
        }
        app.expanded_pane = true;
        assert!(render(&mut app, 140, 40).contains("[Restore]"));
        assert_eq!(app.areas.pane_expanders.len(), 1);
        for (width, height) in [(1, 1), (8, 5), (45, 14)] {
            render(&mut app, width, height);
            for (_, button) in &app.areas.pane_expanders {
                assert!(button.right() <= width && button.bottom() <= height);
            }
        }
    }

    #[test]
    fn timeline_is_optional_and_expansion_uses_the_available_width() {
        let mut app = App::new("http://localhost".into());
        app.push_timeline("retained diagnostic event");
        render(&mut app, 140, 40);
        assert!(!app.areas.panels.iter().any(|p| p.focus == Focus::Timeline));
        let normal = app
            .areas
            .panels
            .iter()
            .find(|p| p.focus == Focus::Source)
            .unwrap()
            .area;
        app.expanded_pane = true;
        render(&mut app, 140, 40);
        assert_eq!(app.areas.panels.len(), 1);
        assert!(app.areas.panels[0].area.width > normal.width);
        app.expanded_pane = false;
        app.show_timeline = true;
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("DDB Timeline"));
        assert!(screen.contains("retained diagnostic event"));
        assert!(app.areas.panels.iter().any(|p| p.focus == Focus::Timeline));
    }

    #[test]
    fn frame_toolbar_control_is_mouse_addressable() {
        let mut app = App::new("http://localhost".into());
        assert!(render(&mut app, 140, 40).contains("Frame F8"));
        let (_, area) = app
            .areas
            .controls
            .iter()
            .find(|(control, _)| *control == Control::RevealFrame)
            .unwrap();
        assert_eq!(
            app.areas.control_at(area.x, area.y),
            Some(Control::RevealFrame)
        );
    }

    #[test]
    fn source_loading_is_visible_until_completion_or_failure() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Source;
        let request = app
            .source_browser
            .navigate(
                None,
                v2::SourceLocation {
                    path: Some("/src/pending.rs".into()),
                    line: 1,
                    ..Default::default()
                },
                crate::source_browser::Navigation::Manual,
            )
            .unwrap();
        assert!(render(&mut app, 140, 40).contains("Loading source"));
        app.source_browser
            .fail_with_error(request.generation, "Source could not be resolved".into());
        let failed = render(&mut app, 140, 40);
        assert!(!failed.contains("Loading source"));
        assert!(failed.contains("Source could not be resolved"));
        app.source_browser.set_following(true);
        crate::source_browser::tests::show_source(
            &mut app.source_browser,
            v2::SourceContent {
                source: Some(v2::SourceFile {
                    source_reference: "source/ready".into(),
                    path: Some("/src/ready.rs".into()),
                    ..Default::default()
                }),
                start_line: 1,
                line_count: 1,
                total_lines: 1,
                content: "ready_source_line".into(),
                ..Default::default()
            },
            1,
        );
        assert!(!render(&mut app, 140, 40).contains("Loading source"));
        assert!(render(&mut app, 140, 40).contains("ready_source_line"));
        let pending = app.source_browser.go_to_line(1).unwrap();
        let loading = render(&mut app, 140, 40);
        assert!(loading.contains("Loading source"));
        assert!(loading.contains("ready_source_line"));
        app.source_browser.fail(pending.generation);
        assert!(!render(&mut app, 140, 40).contains("Loading source"));
        render(&mut app, 1, 1);
    }

    #[test]
    fn memory_pane_reaches_last_row_and_ascii_then_reclamps_on_expand() {
        let mut app = App::new("http://localhost".into());
        let mut bytes = vec![b'A'; 80 * 16];
        bytes[79 * 16..].copy_from_slice(b"LAST_MEMORY_ROW!");
        app.apply_memory(v2::MemoryBlock {
            address: "0x1000".into(),
            data: bytes,
            ..Default::default()
        });
        app.focus = Focus::Memory;
        app.move_selection(79);
        assert!(render(&mut app, 140, 40).contains("00000000000014f0"));
        let pane = app
            .areas
            .panels
            .iter()
            .find(|panel| panel.focus == Focus::Memory)
            .unwrap();
        assert!(pane.first_item > 0);
        assert_eq!(
            app.areas.item_at(pane.area.x + 1, pane.area.y + 1),
            Some((Focus::Memory, pane.first_item))
        );
        app.memory_viewport.scroll(isize::MAX);
        assert!(render(&mut app, 140, 40).contains("|LAST_MEMORY_ROW!|"));
        assert!(app.memory_viewport.column() > 0);
        app.expanded_pane = true;
        let expanded = render(&mut app, 140, 40);
        assert_eq!(app.memory_viewport.column(), 0);
        assert!(expanded.contains("00000000000014f0"));
        assert!(expanded.contains("|LAST_MEMORY_ROW!|"));
        assert_eq!(app.areas.panels.len(), 1);
        assert_eq!(app.areas.panels[0].focus, Focus::Memory);
        render(&mut app, 1, 1);
        app.apply_memory(v2::MemoryBlock::default());
        assert_eq!(app.selected_memory_row, 0);
        assert_eq!(app.memory_viewport.column(), 0);
    }

    #[test]
    fn memory_rendering_handles_address_overflow_without_panicking() {
        let mut app = App::new("http://localhost".into());
        app.apply_memory(v2::MemoryBlock {
            address: "0xfffffffffffffff8".into(),
            data: vec![0x41; 32],
            ..Default::default()
        });
        app.focus = Focus::Memory;
        app.expanded_pane = true;
        assert!(render(&mut app, 140, 40).contains("0xfffffffffffffff8+10"));
    }

    #[test]
    fn register_horizontal_offset_reclamps_when_expanded() {
        let mut app = App::new("http://localhost".into());
        app.apply_registers(vec![v2::Register {
            name: "vector".into(),
            value: format!("{}END", "界".repeat(30)),
            ..Default::default()
        }]);
        app.focus = Focus::Registers;
        render(&mut app, 140, 40);
        app.register_viewport.scroll(isize::MAX);
        assert!(app.register_viewport.column() > 0);
        assert!(render(&mut app, 140, 40).contains("END"));
        app.expanded_pane = true;
        let screen = render(&mut app, 140, 40);
        assert_eq!(app.register_viewport.column(), 0);
        // TestBackend retains an empty cell after each double-width glyph.
        assert!(screen.contains(&format!("vector {}END", "界 ".repeat(30))));
    }

    #[test]
    fn register_pane_has_independent_mouse_focus() {
        let mut app = App::new("http://localhost".into());
        app.apply_registers(
            (0..100)
                .map(|n| v2::Register {
                    name: format!("r{n}"),
                    value: format!("value{n}"),
                    ..Default::default()
                })
                .collect(),
        );
        render(&mut app, 140, 40);
        // The register pane occupies the lower part of the inspection column.
        assert_eq!(app.areas.panel_at(130, 30), Some(Focus::Registers));
        app.focus = Focus::Registers;
        app.move_selection(99);
        assert!(render(&mut app, 140, 40).contains("value99"));
        let pane = app
            .areas
            .panels
            .iter()
            .find(|p| p.focus == Focus::Registers)
            .unwrap();
        assert!(pane.first_item > 0);
        assert_eq!(
            app.areas.item_at(pane.area.x + 1, pane.area.y + 1),
            Some((Focus::Registers, pane.first_item))
        );
        app.expanded_pane = true;
        assert!(render(&mut app, 140, 40).contains("value99"));
        assert_eq!(app.areas.panels.len(), 1);
        assert_eq!(app.areas.panels[0].focus, Focus::Registers);
        app.apply_registers(Vec::new());
        assert_eq!(app.selected_register, 0);
        render(&mut app, 30, 10);
    }

    #[test]
    fn help_wraps_scrolls_and_reclamps_after_resize() {
        let mut app = App::new("http://localhost".into());
        app.show_help = true;
        let first = render(&mut app, 45, 14);
        assert!(first.contains("DDB TUI shortcuts"));
        assert!(first.contains("Esc or ? closes help"));
        assert!(app.help_max_scroll > 0);
        app.help_scroll = usize::MAX;
        let last = render(&mut app, 45, 14);
        assert_eq!(app.help_scroll, app.help_max_scroll);
        assert!(last.contains("Press ? or Esc to close"));
        render(&mut app, 200, 100);
        assert_eq!(app.help_scroll, 0);
        assert_eq!(app.help_max_scroll, 0);
        for (width, height) in [(1, 1), (10, 3), (20, 8)] {
            render(&mut app, width, height);
        }
    }

    #[test]
    fn missing_source_is_visible_in_source_pane() {
        let mut app = App::new("http://localhost".into());
        app.expanded_pane = true;
        app.source_browser
            .unavailable("/build/greeter_server.cc: permission denied".into());
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Source unavailable"));
        assert!(screen.contains("permission denied"));
        assert!(screen.contains("F8: retry selected frame"));
    }

    #[test]
    fn public_extension_descriptor_and_payload_render_generically() {
        use ddb_api_extension::ExtensionProvider;
        use ddb_sample_extension::SampleWorkersExtension;

        let provider = SampleWorkersExtension::default();
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.apply_capabilities(v2::Capabilities {
            api_version: "v2".to_string(),
            schema_version: "2.0.0-draft.3".to_string(),
            extensions: vec![provider.descriptor()],
            ..Default::default()
        });
        app.apply_snapshot(v2::Snapshot {
            extension_states: vec![v2::ExtensionState {
                extension_state_id: "extension-state/workers".to_string(),
                extension_id: ddb_sample_extension::EXTENSION_ID.to_string(),
                revision: 1,
                payloads: provider.state().unwrap(),
            }],
            ..Default::default()
        });
        assert_eq!(
            app.extension_panels[0].rows[0],
            vec!["alpha".to_string(), "session-7".to_string()]
        );
        assert_eq!(
            app.extension_panels[1].rows[0],
            vec!["workers".to_string(), "2".to_string()]
        );
        assert_eq!(app.extension_panels[2].rows[0], vec!["workers"]);
        assert_eq!(
            app.extension_panels[3].rows[0],
            vec!["sample provider ready"]
        );
        assert!(app.extension_panels[4].rows[0][0].contains("move_worker"));
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("Extensions"));
        assert!(screen.contains("Action 1/1"));
        assert_eq!(app.areas.extension_action_rows.len(), 1);
        assert!(screen.contains("Worker placement · Placement"));
        assert!(screen.contains("Worker: alpha"));
        assert!(screen.contains("session-7"));

        app.extension_scroll = 4;
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("Summary"));
        assert!(screen.contains("Key: workers"));

        app.extension_scroll = 8;
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("Topology"));

        app.extension_scroll = usize::MAX;
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("sample provider ready"));
        assert!(screen.contains("move_worker"));
    }

    #[test]
    fn compact_layout_renders_at_supported_terminal_sizes() {
        for (width, height) in [(40, 10), (60, 18), (89, 23), (99, 27)] {
            let mut app = App::new("http://127.0.0.1:5000".to_string());
            let screen = render(&mut app, width, height);
            assert!(
                screen.contains("DDB Debugger"),
                "{width}x{height}: {screen:?}"
            );
            assert!(screen.contains("Source"), "{width}x{height}: {screen:?}");
        }
    }

    #[test]
    fn distributed_stack_truncation_remains_visible_in_panel_title() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.distributed_stack_truncation = Some("max_frames limit reached".to_string());

        let screen = render(&mut app, 140, 40);
        assert!(
            screen.contains("DDB Distributed Call Stack · TRUNCATED"),
            "{screen}"
        );
    }

    #[test]
    fn timeline_keeps_the_newest_wrapped_event_visible() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.show_timeline = true;
        app.push_timeline(format!("receipt {}", "x".repeat(500)));
        for index in 1..=5 {
            app.push_timeline(format!("event {index}"));
        }
        assert!(render(&mut app, 140, 40).contains("event 5"));
    }

    #[test]
    fn execution_navigation_and_breakpoint_markers_are_distinct() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.threads = vec![
            crate::model::ThreadItem {
                id: "thread/alpha".to_string(),
                session_id: "session/alpha".to_string(),
                name: "alpha".to_string(),
                state: "stopped".to_string(),
                function: "main".to_string(),
                file: Some("/src/main.rs".to_string()),
                line: Some(2),
            },
            crate::model::ThreadItem {
                id: "thread/beta".to_string(),
                session_id: "session/beta".to_string(),
                name: "beta".to_string(),
                state: "stopped".to_string(),
                function: "worker".to_string(),
                file: Some("/src/main.rs".to_string()),
                line: Some(1),
            },
        ];

        app.apply_frames(vec![v2::Frame {
            frame_id: "frame/0".to_string(),
            thread_id: "thread/alpha".to_string(),
            function_name: Some("main".to_string()),
            location: Some(v2::SourceLocation {
                path: Some("/src/main.rs".to_string()),
                line: 2,
                ..Default::default()
            }),
            ..Default::default()
        }]);
        crate::source_browser::tests::show_source(
            &mut app.source_browser,
            v2::SourceContent {
                source: Some(v2::SourceFile {
                    source_reference: "source/main".to_string(),
                    debugger_path: Some("/src/main.rs".into()),
                    owners: ["session/alpha", "session/beta"]
                        .into_iter()
                        .map(|session_id| v2::SourceOwner {
                            session_id: session_id.into(),
                            ..Default::default()
                        })
                        .collect(),
                    path: Some("/src/main.rs".to_string()),
                    name: "main.rs".to_string(),
                    media_type: "text/plain".to_string(),
                    content_hash: None,
                    ..Default::default()
                }),
                start_line: 1,
                content: format!(
                    "first\nexecuting{}RIGHT_EDGE\nselected\nlast",
                    "x".repeat(150)
                ),
                line_count: 4,
                has_more: false,
                total_lines: 4,
            },
            2,
        );
        assert!(app.source_browser.move_cursor(1).is_none());
        app.breakpoints.push(Breakpoint {
            id: "breakpoint/1".to_string(),
            target: Some(crate::api::session_target("session/alpha")),
            location: BreakpointLocation {
                src: "/src/main.rs".to_string(),
                line: 4,
            },
            enabled: true,
            times: 0,
            condition: None,
            ignore_count: Some(2),
            temporary: false,
            hardware: false,
            verified: true,
            pending: false,
            message: Some("waiting for worker session".to_string()),
            sub_breakpoints: Vec::new(),
        });
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("◆       1 │ first"), "{screen}");
        assert!(screen.contains("▶       2 │ executing"), "{screen}");
        assert!(screen.contains("  ▸     3 │ selected"), "{screen}");
        assert!(screen.contains(" ●      4 │ last"), "{screen}");
        assert!(!screen.contains("RIGHT_EDGE"));
        let cursor = app.source_browser.document().unwrap().cursor;
        app.source_browser.scroll_horizontal(isize::MAX);
        let shifted = render(&mut app, 140, 40);
        assert!(shifted.contains("RIGHT_EDGE"), "{shifted}");
        assert!(shifted.contains("▶       2 │"), "{shifted}");
        assert!(shifted.contains("  ▸     3 │"), "{shifted}");
        assert!(shifted.contains(" ●      4 │"), "{shifted}");
        assert_eq!(app.source_browser.document().unwrap().cursor, cursor);
        app.focus = Focus::Breakpoints;
        let breakpoint_screen = render(&mut app, 120, 24);
        assert!(
            breakpoint_screen.contains("ignore next 2"),
            "{breakpoint_screen}"
        );
        assert!(
            breakpoint_screen.contains("waiting for worker session"),
            "{breakpoint_screen}"
        );
        app.breakpoints[0].message = Some(format!("{}BREAKPOINT_END", "x".repeat(100)));
        assert!(!render(&mut app, 45, 14).contains("BREAKPOINT_END"));
        let hit_rows = app.areas.breakpoint_sites.clone();
        app.breakpoint_viewport.scroll(isize::MAX);
        assert!(render(&mut app, 45, 14).contains("BREAKPOINT_END"));
        assert_eq!(app.areas.breakpoint_sites, hit_rows);
        assert_eq!(app.selected_breakpoint, 0);
    }
    #[test]
    fn breakpoint_current_hit_marker_clears_on_resume_despite_historical_hits() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Breakpoints;
        app.apply_snapshot(v2::Snapshot {
            sessions: vec![v2::Session {
                session_id: "session/hit".into(),
                ..Default::default()
            }],
            breakpoints: vec![v2::Breakpoint {
                breakpoint_id: "breakpoint/hit".into(),
                hit_count: 15,
                spec: Some(v2::BreakpointSpec {
                    enabled: Some(true),
                    location: Some(v2::breakpoint_spec::Location::Function(
                        v2::FunctionBreakpointLocation {
                            function_name: "hit_here".into(),
                        },
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            execution_states: vec![v2::ExecutionState {
                execution_state_id: "execution/hit".into(),
                target: Some(crate::api::thread_target("thread/hit")),
                revision: 2,
                stop_reason: Some(v2::StopReason {
                    kind: v2::StopReasonKind::Breakpoint as i32,
                    breakpoint_id: Some("breakpoint/hit".into()),
                    thread_id: Some("thread/hit".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.apply_threads(vec![v2::Thread {
            thread_id: "thread/hit".into(),
            session_id: "session/hit".into(),
            state: v2::ThreadState::Stopped as i32,
            ..Default::default()
        }]);
        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("▶ 1 paused"), "{screen}");
        assert!(screen.contains("hits:15"));
        app.open_breakpoint_hits().unwrap();
        assert!(render(&mut app, 100, 24).contains("Paused breakpoint hits"));
        assert_eq!(app.areas.breakpoint_hit_rows.len(), 1);
        assert!(app.areas.breakpoint_hit_cancel.is_some());
        for (width, height) in [(1, 1), (8, 5), (45, 14)] {
            render(&mut app, width, height);
            for (_, row) in &app.areas.breakpoint_hit_rows {
                assert!(row.right() <= width && row.bottom() <= height);
            }
        }
        app.breakpoint_hit_picker = None;
        app.mark_running(Some("thread/hit"));
        let screen = render(&mut app, 100, 24);
        assert!(!screen.contains("paused"), "{screen}");
        assert!(screen.contains("hits:15"));
    }

    #[test]
    fn topology_horizontal_scroll_reveals_long_session_labels_without_changing_target() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Threads;
        app.apply_snapshot(v2::Snapshot {
            sessions: vec![v2::Session {
                session_id: "session/long".into(),
                display_name: format!("{}SESSION_END", "worker_".repeat(20)),
                ..Default::default()
            }],
            ..Default::default()
        });
        let target = app.execution_target.clone();
        assert!(!render(&mut app, 45, 14).contains("SESSION_END"));
        let selected = app.selected_topology_row();
        app.topology_viewport.scroll(isize::MAX);
        assert!(render(&mut app, 45, 14).contains("SESSION_END"));
        assert_eq!(app.selected_topology_row(), selected);
        assert_eq!(app.execution_target, target);
        render(&mut app, 200, 24);
        assert_eq!(app.topology_viewport.column(), 0);
    }

    #[test]
    fn ddb_topology_and_target_picker_are_visible_and_mouse_addressable() {
        let mut app = App::new("http://127.0.0.1:5000".to_string());
        app.focus = Focus::Threads;
        app.apply_snapshot(v2::Snapshot {
            groups: vec![v2::Group {
                group_id: "group/workers".to_string(),
                display_name: "workers".to_string(),
                revision: 1,
                ..Default::default()
            }],
            sessions: vec![v2::Session {
                session_id: "session/worker-1".to_string(),
                display_name: "worker-1".to_string(),
                group_id: Some("group/workers".to_string()),
                revision: 1,
                ..Default::default()
            }],
            ..Default::default()
        });
        app.apply_threads(vec![v2::Thread {
            thread_id: "thread/worker-1".to_string(),
            session_id: "session/worker-1".to_string(),
            name: Some("main".to_string()),
            state: v2::ThreadState::Stopped as i32,
            revision: 1,
            ..Default::default()
        }]);
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("DDB Topology"), "{screen}");
        assert!(screen.contains("workers"), "{screen}");
        assert!(screen.contains("worker-1"), "{screen}");

        app.breakpoint_target_picker = Some(BreakpointTargetPicker {
            draft: BreakpointDraft {
                location: v2::breakpoint_spec::Location::Source(v2::SourceBreakpointLocation {
                    source: "/src/main.rs".into(),
                    line: 12,
                    column: 0,
                }),
                options: BreakpointOptions::default(),
            },
            choices: vec![
                BreakpointTargetChoice {
                    target: BreakpointTarget::Broadcast,
                    label: "All eligible DDB sessions".to_string(),
                    detail: "server-resolved broadcast".to_string(),
                    selected: false,
                },
                BreakpointTargetChoice {
                    target: BreakpointTarget::Group("group/workers".to_string()),
                    label: "Group · workers".to_string(),
                    detail: "1 session".to_string(),
                    selected: true,
                },
            ],
            cursor: 1,
        });
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("DDB breakpoint targets"), "{screen}");
        assert!(screen.contains("[×] Group · workers"), "{screen}");
        assert!(screen.contains("[ Apply ]"), "{screen}");
        let apply = app.areas.breakpoint_target_apply.unwrap();
        assert!(app.areas.breakpoint_target_apply_at(apply.x, apply.y));
        let (_, row) = app.areas.breakpoint_target_rows[1];
        assert_eq!(app.areas.breakpoint_target_at(row.x, row.y), Some(1));
    }
}
