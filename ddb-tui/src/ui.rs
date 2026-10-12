#[path = "source_explorer_ui.rs"]
mod explorer;
#[path = "toolbar_ui.rs"]
mod toolbar;

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
    model::{App, Control, Focus, InputMode, MemberState, TopologyRowKind, UiAreas},
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
        draw_session_termination(frame, app, root, &mut areas);
        draw_topology_details(frame, app, root, &mut areas);
        return areas;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(toolbar::height(app, root.width)),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(root);

    let mut areas = UiAreas::default();
    toolbar::draw(frame, app, rows[0], &mut areas);

    if app.expanded_pane || root.width < 100 || root.height < 28 {
        draw_compact_panel(frame, app, rows[1], &mut areas);
        draw_pane_expanders(frame, app, &mut areas);
        draw_prompt(frame, app, rows[2]);
        draw_hints(frame, app, rows[3]);
        if app.show_help {
            draw_help(frame, app, root);
        }
        draw_signal_picker(frame, app, root, &mut areas);
        draw_breakpoint_hits(frame, app, root, &mut areas);
        draw_breakpoint_target_picker(frame, app, root, &mut areas);
        explorer::draw(frame, app, root, &mut areas);
        draw_session_termination(frame, app, root, &mut areas);
        draw_topology_details(frame, app, root, &mut areas);
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

    draw_threads(frame, app, left[0], &mut areas);
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
    areas.add_panel(
        Focus::Source,
        source_hit_area(&areas, center[0]),
        source_start(app),
        1,
    );
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
    draw_hints(frame, app, rows[3]);
    if app.show_help {
        draw_help(frame, app, root);
    }
    draw_breakpoint_target_picker(frame, app, root, &mut areas);
    draw_signal_picker(frame, app, root, &mut areas);
    draw_breakpoint_hits(frame, app, root, &mut areas);
    explorer::draw(frame, app, root, &mut areas);
    draw_session_termination(frame, app, root, &mut areas);
    draw_topology_details(frame, app, root, &mut areas);
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
        if panel.focus == Focus::Variables && panel.area.width >= 24 {
            let button = Rect::new(panel.area.x + 1, panel.area.bottom() - 1, 8, 1);
            frame.render_widget(
                Paragraph::new("Edit [W]").style(Style::default().fg(ACCENT)),
                button,
            );
            areas.watch_edit = Some(button);
        }
        if panel.focus == Focus::Threads && panel.area.width >= 24 {
            let button = Rect::new(panel.area.x + 1, panel.area.bottom() - 1, 8, 1);
            frame.render_widget(
                Paragraph::new("Kill [K]").style(Style::default().fg(RED)),
                button,
            );
            areas.termination_open = Some(button);
        }
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
                "{} · {}{}",
                session_display_name(app, &hit.session_id),
                name,
                if app.trace_api {
                    format!(" · {} · stop {}", hit.thread_id, hit.revision)
                } else {
                    String::new()
                }
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
    if app.has_activity() || app.operations.display().is_some() {
        draw_operation_feedback(frame, app, area);
        return;
    }
    let state = if app.api_connected {
        "connected"
    } else {
        "reconnecting"
    };
    let detail = if app.trace_api {
        format!(
            " · {} · {}",
            app.api_protocol,
            endpoint_label(&app.api_endpoint)
        )
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(terminal_text(&format!(" DDB Debugger · {state}{detail} ")))
            .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_mini_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.input_mode != InputMode::Normal {
        draw_prompt(frame, app, area);
        return;
    }
    frame.render_widget(
        Paragraph::new(terminal_text(&format!(" {} ", app.status)))
            .style(Style::default().fg(MUTED)),
        area,
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
        Focus::Threads => draw_threads(frame, app, area, areas),
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
        Focus::Source => (source_hit_area(areas, area), source_start(app), 1),
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

fn draw_threads(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    let rows = app.topology_rows();
    let selected = app.selected_topology_row();
    let lines = rows
        .iter()
        .map(|row| {
            let (icon, label_color) = match row.kind {
                TopologyRowKind::Group(_) => (
                    if app.topology_collapsed(&row.kind) {
                        "▸"
                    } else {
                        "▾"
                    },
                    ACCENT,
                ),
                TopologyRowKind::Session(_) => (
                    if app.topology_can_fold(&row.kind) {
                        if app.topology_collapsed(&row.kind) {
                            "▸ ◇"
                        } else {
                            "▾ ◇"
                        }
                    } else {
                        "◇"
                    },
                    Color::White,
                ),
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
    let first = list_first(selected, rows.len(), area, 1);
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    for (offset, row) in rows
        .iter()
        .skip(first)
        .take(inner.height as usize)
        .enumerate()
    {
        let y = inner.y + offset as u16;
        areas
            .topology_rows
            .push((row.kind.clone(), Rect::new(inner.x, y, inner.width, 1)));
        let icon_column = row.depth * 2;
        let left = app.topology_viewport.column();
        if app.topology_can_fold(&row.kind) && icon_column >= left {
            let offset = icon_column - left + 2; // List's selection marker occupies two columns.
            if offset < inner.width as usize {
                areas.topology_folds.push((
                    row.kind.clone(),
                    Rect::new(
                        inner.x + offset as u16,
                        y,
                        (inner.width - offset as u16).min(2),
                        1,
                    ),
                ));
            }
        }
    }
    if area.width >= 26 && area.height > 0 {
        let control = Rect::new(area.right() - 12, area.y, 11, 1);
        frame.render_widget(
            Paragraph::new(if app.topology_grouped {
                " Flat [v] "
            } else {
                " Group [v] "
            })
            .style(Style::default().fg(ACCENT)),
            control,
        );
        areas.topology_grouping = Some(control);
    }
}

fn draw_breakpoints(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    if app.breakpoint_hit_view.visible {
        draw_current_hits(frame, app, area, areas);
        return;
    }
    let mut rows = Vec::new();
    let hits = app.breakpoint_hits();
    let mut member_states = std::collections::HashMap::new();
    let mut previous_file = None;
    for index in app.breakpoint_order() {
        let breakpoint = &app.breakpoints[index];
        if app.breakpoints_grouped && previous_file != Some(breakpoint.file_group()) {
            rows.push((
                None,
                None,
                Line::styled(
                    breakpoint
                        .file_group()
                        .map(|file| format!("File: {}", terminal_text(file)))
                        .unwrap_or_else(|| "Non-source / unresolved breakpoints".into()),
                    Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                ),
                None,
            ));
            previous_file = Some(breakpoint.file_group());
        }
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
            Some(index),
            None,
            Line::from(vec![
                Span::styled(
                    if breakpoint.enabled { "● " } else { "○ " },
                    Style::default().fg(if breakpoint.enabled { RED } else { MUTED }),
                ),
                Span::styled(
                    if app.logpoint_ids.contains(&breakpoint.id) {
                        "log "
                    } else {
                        ""
                    },
                    Style::default().fg(GREEN),
                ),
                Span::styled(
                    if breakpoint.location.line > 0 {
                        format!(
                            "{}:{} ",
                            terminal_text(short_path(&breakpoint.location.src)),
                            breakpoint.location.line
                        )
                    } else {
                        format!("{} ", terminal_text(&breakpoint.location.src))
                    },
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    format!("total hits:{}{}", breakpoint.times, attributes),
                    Style::default().fg(MUTED),
                ),
                Span::styled(
                    format!(" · {scope} · {} sites", breakpoint.sub_breakpoints.len()),
                    Style::default().fg(MUTED),
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
            hit_badge(current_hits.len()),
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
                Some(index),
                Some(site),
                Line::from(vec![
                    Span::styled("  ↳ ", Style::default().fg(MUTED)),
                    Span::styled(session, Style::default().fg(Color::White)),
                    Span::styled(format!(" · {location} · "), Style::default().fg(MUTED)),
                    Span::styled(
                        if member.verified {
                            "verified"
                        } else {
                            "unverified"
                        },
                        Style::default().fg(if member.verified { GREEN } else { YELLOW }),
                    ),
                    Span::styled(
                        format!(" · total hits:{}{message}", member.hit_count),
                        Style::default().fg(MUTED),
                    ),
                ]),
                Some(hit_badge(site_hits).unwrap_or_else(|| {
                    member_badge(
                        *member_states
                            .entry(member.session_id.as_str())
                            .or_insert_with(|| app.breakpoint_member_state(&member.session_id)),
                    )
                })),
            ));
        }
    }
    let selected = rows
        .iter()
        .position(|(index, site, _, _)| {
            *index == Some(app.selected_breakpoint) && *site == app.selected_breakpoint_site
        })
        .unwrap_or(0);
    let first = list_first(selected, rows.len(), area, 1);
    app.breakpoint_viewport.fit(
        rows.iter()
            .map(|(_, _, line, badge)| {
                line.width() + badge.as_ref().map_or(0, |badge| badge.width() + 1)
            })
            .max()
            .unwrap_or(0),
        area.width.saturating_sub(4) as usize,
    );
    let parents = rows
        .iter()
        .map(|(index, site, _, _)| (*index, *site))
        .collect::<Vec<_>>();
    let items = rows
        .into_iter()
        .map(|(_, _, line, badge)| {
            ListItem::new(breakpoint_line(
                line,
                badge,
                app.breakpoint_viewport.column(),
                area.width.saturating_sub(4) as usize,
            ))
        })
        .collect::<Vec<_>>();
    draw_selectable_list(
        frame,
        area,
        title(
            app,
            Focus::Breakpoints,
            &if hits.is_empty() {
                "DDB Breakpoints".into()
            } else {
                format!(
                    "DDB Breakpoints · {} {}",
                    hits.len(),
                    if hits.len() == 1 { "hit" } else { "hits" }
                )
            },
        ),
        items,
        selected,
        app.focus == Focus::Breakpoints,
    );
    areas.add_focus_panel(Focus::Breakpoints, area);
    // Keep grouping mouse-accessible without overwriting the title/count.
    // The footer reserves separate cells for Paused, grouping, and Expand.
    if area.width >= 32 && area.height >= 3 {
        let control = Rect::new(area.x + 12, area.bottom() - 1, 9, 1);
        frame.render_widget(
            Paragraph::new(if app.breakpoints_grouped {
                "Flat [v]"
            } else {
                "Files [v]"
            })
            .style(Style::default().fg(ACCENT)),
            control,
        );
        areas.breakpoint_grouping = Some(control);
    }
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let end = (first + inner.height as usize).min(parents.len());
    for (row, (parent, site)) in parents[first..end].iter().enumerate() {
        let Some(parent) = parent else {
            continue;
        };
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
    draw_hit_view_toggle(frame, app, area, areas);
}

fn hit_badge(hits: usize) -> Option<Span<'static>> {
    (hits > 0).then(|| {
        Span::styled(
            format!("[{hits} {}]", if hits == 1 { "hit" } else { "hits" }),
            Style::default().fg(YELLOW).bg(Color::Rgb(58, 51, 29)),
        )
    })
}

fn member_badge(state: MemberState) -> Span<'static> {
    use v2::StopReasonKind as Reason;
    let (label, color) = match state {
        MemberState::Offline => ("offline", MUTED),
        MemberState::Unavailable => ("unavailable", MUTED),
        MemberState::Unknown => ("unknown", MUTED),
        MemberState::Running => ("running", GREEN),
        MemberState::Mixed => ("mixed", YELLOW),
        MemberState::Stopped(Reason::Step) => ("stepped", ACCENT),
        MemberState::Stopped(Reason::Breakpoint) => ("bp stop", YELLOW),
        MemberState::Stopped(Reason::Watchpoint) => ("watchpoint", YELLOW),
        MemberState::Stopped(Reason::Signal) => ("signal", RED),
        MemberState::Stopped(Reason::Exception) => ("exception", RED),
        MemberState::Stopped(Reason::Entry) => ("entry", ACCENT),
        MemberState::Stopped(Reason::Exit) => ("exited", MUTED),
        MemberState::Stopped(Reason::Pause) => ("paused", MUTED),
        MemberState::Stopped(Reason::Other) => ("stopped", MUTED),
        MemberState::Stopped(Reason::Unspecified) => ("unknown", MUTED),
    };
    Span::styled(format!("[{label}]"), Style::default().fg(color))
}

fn breakpoint_line(
    line: Line<'static>,
    badge: Option<Span<'static>>,
    column: usize,
    width: usize,
) -> Line<'static> {
    let line = horizontal_line(line, column);
    let Some(badge) = badge else { return line };
    let badge_width = badge.width();
    if width < badge_width + 4 {
        return crate::display_text::truncate_line(line, width);
    }
    let label_width = width - badge_width - 1;
    let mut line = crate::display_text::truncate_line(line, label_width);
    line.spans
        .push(Span::raw(" ".repeat(width - badge_width - line.width())));
    line.spans.push(badge);
    line
}

fn draw_hit_view_toggle(frame: &mut Frame<'_>, app: &App, area: Rect, areas: &mut UiAreas) {
    if area.width >= 24 && area.height >= 3 {
        let button = Rect::new(area.x + 1, area.bottom() - 1, 10, 1);
        frame.render_widget(
            Paragraph::new(if app.breakpoint_hit_view.visible {
                "All [H]"
            } else {
                "Paused [H]"
            })
            .style(Style::default().fg(ACCENT)),
            button,
        );
        areas.breakpoint_hit_view_toggle = Some(button);
    }
}

fn draw_current_hits(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    let rows = app.hit_view_rows();
    let current = app.current_visible_hit();
    let selected = rows
        .iter()
        .position(|row| row.hit.as_ref().is_some_and(|hit| Some(hit) == current));
    let first = list_first(selected.unwrap_or(0), rows.len(), area, 1);
    let lines = rows
        .iter()
        .map(|row| {
            Line::from(vec![
                Span::raw("  ".repeat(row.depth)),
                Span::styled(
                    terminal_text(&row.label),
                    Style::default().fg(if row.hit.is_some() {
                        YELLOW
                    } else if row.depth == 0 {
                        ACCENT
                    } else {
                        Color::White
                    }),
                ),
            ])
        })
        .collect::<Vec<_>>();
    app.breakpoint_viewport.fit(
        lines.iter().map(Line::width).max().unwrap_or(0),
        area.width.saturating_sub(4) as usize,
    );
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.api_connected {
                "No current breakpoint hits\n\nOnly threads paused at a breakpoint appear here.\nH: all breakpoints"
            } else {
                "Disconnected: no verified current hits\n\nH: all breakpoints"
            }).block(panel_block(&title(app, Focus::Breakpoints, "DDB Breakpoint Hits"), app.focus == Focus::Breakpoints))
                .style(Style::default().fg(MUTED)), area,
        );
    } else {
        draw_selectable_list(
            frame,
            area,
            title(app, Focus::Breakpoints, "DDB Breakpoint Hits"),
            lines
                .into_iter()
                .map(|line| ListItem::new(horizontal_line(line, app.breakpoint_viewport.column())))
                .collect(),
            selected,
            app.focus == Focus::Breakpoints,
        );
    }
    areas.add_focus_panel(Focus::Breakpoints, area);
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    for (offset, row) in rows
        .iter()
        .skip(first)
        .take(inner.height as usize)
        .enumerate()
    {
        if let Some(hit) = row.hit.as_ref() {
            areas.current_hit_rows.push((
                hit.clone(),
                Rect::new(inner.x, inner.y + offset as u16, inner.width, 1),
            ));
        }
    }
    draw_hit_view_toggle(frame, app, area, areas);
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
    let search_visible =
        !app.source_search.query.is_empty() || app.input_mode == InputMode::SourceSearch;
    let inner = if search_visible && inner.height >= 2 {
        let search_rows =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
        let buttons =
            Layout::horizontal([Constraint::Length(9), Constraint::Min(0)]).split(search_rows[1]);
        frame.render_widget(
            Paragraph::new(format!("[{}]", app.source_search.mode.label()))
                .style(Style::default().fg(ACCENT)),
            buttons[0],
        );
        let status = app
            .source_browser
            .document()
            .and_then(|document| app.source_search.status(&document.source.id()))
            .unwrap_or("/ edit · Ctrl+f toggles mode in prompt");
        let query = if app.input_mode == InputMode::SourceSearch {
            &app.input
        } else {
            &app.source_search.query
        };
        frame.render_widget(
            Paragraph::new(terminal_text(&format!("{status} · /{query}")))
                .style(Style::default().fg(YELLOW)),
            buttons[1],
        );
        areas.source_search_mode = Some(buttons[0]);
        areas.source_search_bar = Some(search_rows[1]);
        search_rows[0]
    } else {
        inner
    };
    if area.width >= 12 && area.height > 0 {
        let open = Rect::new(area.right() - 11, area.y, 10, 1);
        frame.render_widget(
            Paragraph::new(" Open [o] ").style(Style::default().fg(ACCENT)),
            open,
        );
        areas.source_explorer_open = Some(open);
    }
    if area.width >= 26 && area.height > 0 {
        let inspect = Rect::new(area.right() - 25, area.y, 13, 1);
        frame.render_widget(
            Paragraph::new(" Inspect [E] ").style(Style::default().fg(ACCENT)),
            inspect,
        );
        areas.source_inspect = Some(inspect);
    }
    if area.width >= 56 && area.height > 0 {
        let search = Rect::new(area.right() - 38, area.y, 12, 1);
        let available = app.source_browser.document().is_some();
        frame.render_widget(
            Paragraph::new(" Search [/] ").style(Style::default().fg(if available {
                ACCENT
            } else {
                MUTED
            })),
            search,
        );
        if available {
            areas.source_search = Some(search);
        }
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
            let mut spans = vec![
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
            ];
            spans.extend(
                source_search_text(
                    text,
                    source.column,
                    app.source_search
                        .highlight(&source.source.id(), line_number, text),
                )
                .spans,
            );
            Line::from(spans).style(style)
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

fn source_hit_area(areas: &UiAreas, mut area: Rect) -> Rect {
    if areas.source_search_bar.is_some() {
        area.height = area.height.saturating_sub(1);
    }
    area
}

fn source_search_text(
    text: &str,
    column: usize,
    highlight: Option<(usize, usize)>,
) -> Line<'static> {
    let text = terminal_text(text);
    let Some((start, end)) = highlight else {
        return horizontal_line(Line::from(text), column);
    };
    let mut parts = [String::new(), String::new(), String::new()];
    let mut position = 0;
    let span = Span::raw(text);
    for grapheme in span.styled_graphemes(Style::default()) {
        let next = position + grapheme.symbol.chars().count();
        let part = if next <= start {
            0
        } else if position < end {
            1
        } else {
            2
        };
        parts[part].push_str(grapheme.symbol);
        position = next;
    }
    let [before, matched, after] = parts;
    horizontal_line(
        Line::from(vec![
            Span::raw(before),
            Span::styled(
                matched,
                Style::default()
                    .fg(YELLOW)
                    .bg(Color::Rgb(70, 60, 20))
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Span::raw(after),
        ]),
        column,
    )
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
                            terminal_text(&if app.trace_api {
                                stack_frame.thread_id.clone()
                            } else {
                                app.target_label(Some(&crate::api::thread_target(
                                    &stack_frame.thread_id,
                                )))
                            })
                        ),
                        Style::default().fg(MUTED),
                    ),
                ]);
            }
            let source = app.frame_source_status(stack_frame);
            Line::from(vec![
                Span::styled(
                    format!("#{:<3}", stack_frame.distributed_index),
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    terminal_text(&stack_frame.function),
                    Style::default().fg(if source.dimmed() { MUTED } else { Color::White }),
                ),
                Span::styled(source.label(), Style::default().fg(MUTED)),
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
                Span::styled(
                    source
                        .error
                        .map_or_else(String::new, |error| format!(" · {}", terminal_text(error))),
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
    let stack_title = if app.stack_mode == crate::cli::StackMode::Local {
        "DDB Local Call Stack"
    } else if app.distributed_stack_truncation.is_some() {
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
    if app.expanded_pane && app.focus == Focus::Variables {
        return (area, None, None);
    }
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
        title(app, Focus::Registers, "Registers · = edit"),
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
    let memory_title = title(app, Focus::Memory, &format!("Raw Memory · {byte_count} B"));
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

fn draw_timeline(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let timeline_title = title(app, Focus::Timeline, "DDB Timeline");
    let block = panel_block(&timeline_title, app.focus == Focus::Timeline);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = app
        .timeline
        .visible(inner.width, inner.height, &mut app.timeline_scroll)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::Gray))))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_extensions(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
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
    areas.extension_data = Some(inner);
    let panels = &app.extension_panels;
    let document = app.extension_document.get_or_insert_with(|| {
        let mut lines = Vec::new();
        for panel in panels {
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
        crate::wrapped_text::WrappedText::new(lines)
    });
    let lines = document.visible(inner.width, inner.height, &mut app.extension_scroll);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_extension_actions(frame: &mut Frame<'_>, app: &mut App, area: Rect, areas: &mut UiAreas) {
    let Some(action) = app.extension_actions.get(app.selected_extension_action) else {
        return;
    };
    let position = format!(
        "Action {}/{} · ",
        app.selected_extension_action + 1,
        app.extension_actions.len()
    );
    let line = Line::from(vec![
        Span::styled(position, Style::default().fg(YELLOW)),
        Span::styled(
            format!(
                "{} · {}",
                terminal_text(&action.extension_title),
                terminal_text(&action.title)
            ),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  [←/→ select · Shift+←/→ scroll · a/Enter/click]",
            Style::default().fg(MUTED),
        ),
    ]);
    app.extension_action_viewport
        .fit(line.width(), area.width as usize);
    frame.render_widget(
        Paragraph::new(horizontal_line(
            line,
            app.extension_action_viewport.column(),
        )),
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
        InputMode::EditWatch => ("Edit watch expression (Esc cancels)", "W> ", GREEN),
        InputMode::ExportDetails => (
            "Export snapshot to NEW file (Enter saves, Esc cancels)",
            "path> ",
            GREEN,
        ),
        InputMode::InspectExpression => {
            ("Inspect once in selected frame (Esc cancels)", "E> ", GREEN)
        }
        InputMode::AssignValue => ("Assign selected value (Esc cancels)", "= ", YELLOW),
        InputMode::Logpoint => (
            "Log message {expression}; empty restores stopping breakpoint",
            "L> ",
            GREEN,
        ),
        InputMode::Memory => ("Read memory", "m> ", YELLOW),
        InputMode::Jump => ("Jump execution location", "J> ", YELLOW),
        InputMode::Signal => ("Send signal", "s> ", RED),
        InputMode::GotoLine => ("Open source (line or path:line)", "g> ", ACCENT),
        InputMode::SourceSearch => (
            match app.source_search.mode {
                crate::source_search::Mode::Exact => "Search source · exact (Ctrl+f toggles fuzzy)",
                crate::source_search::Mode::Fuzzy => "Search source · fuzzy (Ctrl+f toggles exact)",
            },
            "/ ",
            YELLOW,
        ),
        InputMode::Breakpoint => (
            "Breakpoint (-t -h --ignore-count N if CONDITION)",
            "B> ",
            RED,
        ),
        InputMode::FunctionBreakpoint => ("Function breakpoint (symbol [ -- OPTIONS])", "F> ", RED),
        InputMode::ExtensionAction => ("Extension action JSON", "a> ", ACCENT),
        InputMode::Normal => ("Status", "", MUTED),
    };
    let block = if area.height >= 3 {
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
    } else {
        Block::default()
    };
    let visible_width = usize::from(block.inner(area).width);
    let content = if app.input_mode == InputMode::Normal {
        Line::from(terminal_text(&app.status))
    } else {
        let before = app.input.chars().take(app.input_cursor).collect::<String>();
        let cursor_end = crate::display_text::width(prefix)
            + crate::display_text::width(&before)
            + app
                .input
                .chars()
                .nth(app.input_cursor)
                .map(|character| crate::display_text::width(&character.to_string()).max(1))
                .unwrap_or(1);
        horizontal_line(
            input_line(prefix, &app.input, app.input_cursor, color),
            cursor_end.saturating_sub(visible_width),
        )
    };
    frame.render_widget(
        Paragraph::new(content)
            .style(Style::default().fg(color))
            .block(block),
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

fn draw_operation_feedback(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let color = if app.operations.has_error() {
        RED
    } else {
        YELLOW
    };
    let text = if !app.operations.active() && app.background_activity().is_some() {
        app.background_activity().unwrap().to_string()
    } else if app.trace_api {
        app.operations.diagnostics().unwrap_or_default()
    } else {
        app.operations.display().unwrap_or_default()
    };
    frame.render_widget(
        Paragraph::new(terminal_text(&format!(
            "{} {}",
            if app.has_activity() {
                app.operations.spinner()
            } else {
                app.operations.indicator()
            },
            text
        )))
        .style(Style::default().fg(color)),
        area,
    );
}

fn draw_hints(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.has_activity() || app.operations.display().is_some() {
        draw_operation_feedback(frame, app, area);
        return;
    }
    frame.render_widget(
        Paragraph::new(
            " Alt+1…9/0 panes  z expand  ↑↓ navigate  b/B breakpoint  P inspect hit  ? help ",
        )
        .style(Style::default().fg(MUTED))
        .alignment(Alignment::Center),
        area,
    );
}

fn draw_topology_details(frame: &mut Frame<'_>, app: &mut App, root: Rect, areas: &mut UiAreas) {
    let Some(details) = app.topology_details.as_mut() else {
        return;
    };
    let area = centered(root, 95, 90);
    frame.render_widget(Clear, area);
    let block = panel_block(
        "Topology details · snapshot · y copy · e export · Esc close",
        true,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let status_height = details.copy_status.as_ref().map_or(0, |message| {
        Paragraph::new(terminal_text(message))
            .wrap(Wrap { trim: false })
            .line_count(inner.width)
            .min(usize::from(inner.height.saturating_sub(2))) as u16
    });
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
    let content =
        Layout::vertical([Constraint::Min(0), Constraint::Length(status_height)]).split(rows[0]);
    if let Some(message) = &details.copy_status {
        frame.render_widget(
            Paragraph::new(terminal_text(message)).wrap(Wrap { trim: false }),
            content[1],
        );
    }
    let inner = content[0];
    if rows[1].width >= 18 && rows[1].height > 0 {
        let export = Rect::new(rows[1].x, rows[1].y, 10, 1);
        let close = Rect::new(rows[1].right() - 7, rows[1].y, 7, 1);
        frame.render_widget(
            Paragraph::new("[e Export]").style(Style::default().fg(ACCENT)),
            export,
        );
        frame.render_widget(
            Paragraph::new("[Close]").style(Style::default().fg(ACCENT)),
            close,
        );
        areas.details_export = Some(export);
        areas.details_close = Some(close);
        if rows[1].width >= 29 {
            let copy = Rect::new(rows[1].x + 11, rows[1].y, 8, 1);
            frame.render_widget(
                Paragraph::new("[y Copy]").style(Style::default().fg(ACCENT)),
                copy,
            );
            areas.details_copy = Some(copy);
        }
    } else {
        frame.render_widget(Paragraph::new("y copy · e export"), rows[1]);
    }
    details.page = usize::from(inner.height).max(1);
    details.scroll(0);
    details.horizontal.fit(details.width, inner.width as usize);
    let lines = details
        .lines
        .iter()
        .skip(details.top)
        .take(details.page)
        .map(|line| horizontal_line(Line::from(line.clone()), details.horizontal.column()))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_session_termination(frame: &mut Frame<'_>, app: &mut App, root: Rect, areas: &mut UiAreas) {
    let Some(dialog) = app.session_termination.as_mut() else {
        return;
    };
    let area = centered(root, 85, 75);
    frame.render_widget(Clear, area);
    let block = panel_block("Terminate one session?", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
    let text = format!(
        "SIGKILL terminates this process. This cannot be undone.\n\nSession: {}\nID: {}\nProcess: {}\nDDB instance: {}\n\nOnly this session is targeted, regardless of execution scope.\nUp/Down or wheel scroll. y confirms; Enter/Esc cancels.",
        terminal_text(&dialog.session.display_name),
        terminal_text(&dialog.session.session_id),
        terminal_text(dialog.session.process_id.as_deref().unwrap_or("unknown")),
        terminal_text(&dialog.instance),
    );
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let maximum = paragraph
        .line_count(rows[0].width)
        .saturating_sub(rows[0].height as usize)
        .min(u16::MAX as usize) as u16;
    dialog.scroll = dialog.scroll.min(maximum);
    frame.render_widget(paragraph.scroll((dialog.scroll, 0)), rows[0]);
    if rows[1].width >= 29 && rows[1].height > 0 {
        let cancel = Rect::new(rows[1].x, rows[1].y, 12, 1);
        let confirm = Rect::new(rows[1].right() - 15, rows[1].y, 15, 1);
        frame.render_widget(
            Paragraph::new("[Esc Cancel]").style(Style::default().fg(ACCENT)),
            cancel,
        );
        frame.render_widget(
            Paragraph::new("[y Terminate]").style(Style::default().fg(RED)),
            confirm,
        );
        areas.termination_cancel = Some(cancel);
        areas.termination_confirm = Some(confirm);
    } else {
        frame.render_widget(Paragraph::new("Esc cancel"), rows[1]);
    }
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
    let rows = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if picker.message.is_some() { 3 } else { 0 }),
        Constraint::Length(1),
    ])
    .split(inner);
    if let Some(message) = &picker.message {
        frame.render_widget(
            Paragraph::new(terminal_text(message))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(YELLOW)),
            rows[1],
        );
    }
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
    .split(rows[2]);
    frame.render_widget(
        Paragraph::new("Space toggle · ↑↓ move").style(Style::default().fg(MUTED)),
        actions[0],
    );
    frame.render_widget(
        Paragraph::new("[ Apply ]")
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(
                        if picker.loading || !picker.choices.iter().any(|choice| choice.selected) {
                            MUTED
                        } else {
                            GREEN
                        },
                    )
                    .add_modifier(Modifier::BOLD),
            ),
        actions[1],
    );
    frame.render_widget(
        Paragraph::new("[ Cancel ]")
            .alignment(Alignment::Center)
            .style(Style::default().fg(YELLOW)),
        actions[2],
    );
    if !picker.loading && picker.choices.iter().any(|choice| choice.selected) {
        areas.breakpoint_target_apply = Some(actions[1]);
    }
    areas.breakpoint_target_cancel = Some(actions[2]);
}
fn draw_help(frame: &mut Frame<'_>, app: &mut App, root: Rect) {
    let area = centered(root, 96, 94);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" DDB TUI shortcuts ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).split(inner);
    let entries = crate::help::entries(&app.help_query);
    let mut text = Vec::new();
    let mut section = "";
    for entry in &entries {
        if entry.section != section {
            if !text.is_empty() {
                text.push(Line::from(""));
            }
            text.push(Line::from(Span::styled(
                entry.section,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
            section = entry.section;
        }
        text.push(Line::from(vec![
            Span::styled(format!("  {}  ", entry.keys), Style::default().fg(YELLOW)),
            Span::raw(entry.description),
        ]));
    }
    if entries.is_empty() {
        text.push(Line::from(
            "No matching shortcuts. Press / to edit the filter.",
        ));
    }
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
            "Search /{}{}\n↑↓ PgUp/PgDn wheel | {}/{}\n/ search · Esc or ? closes help",
            terminal_text(&app.help_query),
            if app.help_searching { "▏" } else { "" },
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
    selected: impl Into<Option<usize>>,
    focused: bool,
) {
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(
            selected
                .into()
                .map(|selected| selected.min(items.len() - 1)),
        );
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
        format!("◆ {} {label}", focus.shortcut())
    } else {
        format!("{} {label}", focus.shortcut())
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

    #[test]
    fn source_match_highlighting_preserves_graphemes_and_horizontal_clipping() {
        let line = source_search_text("a🦀e\u{301}z", 0, Some((3, 4)));
        let matched = line
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
            .unwrap();
        assert_eq!(matched.content, "e\u{301}");
        let line = source_search_text("a🦀e\u{301}z", 2, Some((1, 4)));
        assert_eq!(
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            " e\u{301}z"
        );
        assert!(line
            .spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED)));
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        render_lines(app, width, height).replace('\n', "")
    }

    fn render_lines(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.areas = draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width).max(1))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn export_path_prompt_tracks_cursor_in_short_and_normal_layouts() {
        let mut app = App::new("http://localhost".into());
        app.start_input(InputMode::ExportDetails);
        app.insert_input(&format!("/{}destination.json", "界e\u{301}/".repeat(30)));
        for height in [8, 18, 30] {
            assert!(render(&mut app, 45, height).contains("destination.json"));
            app.input_cursor = 0;
            assert!(render(&mut app, 45, height).contains("path> /"));
            app.input_cursor = app.input.chars().count();
        }
    }

    #[test]
    fn topology_details_reach_long_unicode_lines_and_rows_beyond_u16() {
        let mut app = App::new("http://localhost".into());
        app.sessions = vec![v2::Session {
            session_id: "session".into(),
            ..Default::default()
        }];
        app.open_topology_details(0).unwrap();
        let details = app.topology_details.as_mut().unwrap();
        details.lines = (0..70_000)
            .map(|i| format!("{i}: {}END_MARKER", "界e\u{301}".repeat(100)))
            .collect();
        details.width = Line::from(details.lines.last().unwrap().as_str()).width();
        render(&mut app, 90, 18);
        let details = app.topology_details.as_mut().unwrap();
        details.scroll(isize::MAX);
        assert!(details.top > u16::MAX as usize);
        let screen = render(&mut app, 90, 18);
        assert!(screen.contains("69999:"));
        app.topology_details
            .as_mut()
            .unwrap()
            .horizontal
            .scroll(isize::MAX);
        assert!(render(&mut app, 90, 18).contains("END_MARKER"));
        render(&mut app, 12, 5);
        render(&mut app, 140, 40);
        assert!(
            app.topology_details.as_ref().unwrap().top
                <= 70_000 - app.topology_details.as_ref().unwrap().page
        );
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
    fn expanded_variables_use_the_entire_pane_and_exclude_other_inspection_hits() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Variables;
        app.expanded_pane = true;
        app.apply_variables(vec![v2::Variable {
            name: "marker".into(),
            value: "9501".into(),
            ..Default::default()
        }]);
        app.registers.push(crate::model::Register {
            name: "sp".into(),
            value: "0x1234".into(),
            unavailable: false,
        });
        app.apply_memory(v2::MemoryBlock {
            address: "0x1234".into(),
            data: vec![0; 256],
            ..Default::default()
        });
        for (width, height) in [(140, 40), (90, 18)] {
            let screen = render(&mut app, width, height);
            assert!(screen.contains("Variables / Watches") && screen.contains("[Restore]"));
            assert!(!screen.contains("Registers"), "{screen}");
            assert!(!screen.contains("Raw Memory"), "{screen}");
            assert_eq!(app.areas.panels.len(), 1);
            assert_eq!(app.areas.panels[0].focus, Focus::Variables);
            assert!(app.areas.panels[0].area.height > height / 2);
        }
        app.expanded_pane = false;
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("Registers") && screen.contains("Raw Memory"));
        assert_eq!(
            app.memory[0].bytes.len(),
            256,
            "expanding must not discard inspection data"
        );
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
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("Frame source"), "{screen}");
        assert!(!screen.contains("Frame F8"));
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
    fn toolbar_buttons_have_complete_labels_separate_click_targets_and_wrap() {
        let mut app = App::new("http://localhost".into());
        app.sessions.push(v2::Session {
            session_id: "session".into(),
            ..Default::default()
        });
        app.apply_threads(vec![v2::Thread {
            thread_id: "thread".into(),
            session_id: "session".into(),
            state: v2::ThreadState::Stopped as i32,
            ..Default::default()
        }]);
        app.capabilities.execution_actions = vec![
            v2::ExecutionAction::Continue as i32,
            v2::ExecutionAction::Interrupt as i32,
            v2::ExecutionAction::Next as i32,
            v2::ExecutionAction::StepIn as i32,
            v2::ExecutionAction::StepOut as i32,
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
        for width in [140, 100, 80, 45] {
            let screen = render_lines(&mut app, width, 40);
            assert_eq!(app.areas.controls.len(), 8, "{screen}");
            for (control, rect) in &app.areas.controls {
                let line = screen.lines().nth(rect.y as usize).unwrap();
                let button = line
                    .chars()
                    .skip(rect.x as usize)
                    .take(rect.width as usize)
                    .collect::<String>();
                assert!(
                    button.starts_with('[') && button.ends_with(']'),
                    "{control:?}: {button}"
                );
                for x in rect.x..rect.right() {
                    assert_eq!(app.areas.control_at(x, rect.y), Some(*control));
                }
                assert!(app.areas.control_at(rect.right(), rect.y).is_none());
            }
            for label in [
                "Continue",
                "Pause",
                "Step over",
                "Step in",
                "Step out",
                "Frame source",
                "Scope: Auto",
                "Refresh",
            ] {
                assert!(
                    screen.contains(label),
                    "missing {label} at {width}: {screen}"
                );
            }
            let run = app
                .areas
                .controls
                .iter()
                .find(|(c, _)| *c == Control::Continue)
                .unwrap()
                .1;
            let navigation = app
                .areas
                .controls
                .iter()
                .find(|(c, _)| *c == Control::RevealFrame)
                .unwrap()
                .1;
            if width >= 140 {
                assert_eq!(navigation.y, run.y);
                assert!(!screen.contains("Shift+F11"));
                assert!(!screen.contains("Auto: Continue/Pause"));
            } else {
                assert!(navigation.y > run.y);
            }
        }
    }

    #[test]
    fn hit_badges_are_pinned_after_the_label_and_keep_history_scrollable() {
        let label = Line::from(vec![
            Span::styled("● ", Style::default().fg(RED)),
            Span::raw(format!(
                "main.rs:12 {} total hits:17 END",
                "metadata ".repeat(20)
            )),
        ]);
        for width in [18, 29, 80] {
            let row = breakpoint_line(label.clone(), hit_badge(2), 0, width);
            assert!(row.to_string().starts_with("● "));
            assert!(row.to_string().ends_with("[2 hits]"));
            assert_eq!(row.width(), width);
            assert_eq!(row.spans[0].style.fg, Some(RED));
            assert_eq!(row.spans.last().unwrap().style.fg, Some(YELLOW));
            let end = label.width() + "[2 hits]".len() + 1 - width;
            let scrolled = breakpoint_line(label.clone(), hit_badge(2), end, width).to_string();
            assert!(scrolled.contains("END"), "{scrolled}");
            assert!(scrolled.ends_with("[2 hits]"));
        }
        assert_eq!(breakpoint_line(label.clone(), None, 0, 80), label);
        for width in 0..18 {
            assert!(breakpoint_line(label.clone(), hit_badge(99), 0, width).width() <= width);
        }
    }

    #[test]
    fn long_unicode_scope_is_ellipsized_and_tiny_layouts_keep_hits_in_bounds() {
        let mut app = App::new("http://localhost".into());
        app.execution_scope_override = true;
        app.execution_target = Some(crate::api::group_target("group"));
        app.groups.push(v2::Group {
            group_id: "group".into(),
            display_name: "界e\u{301}".repeat(80),
            ..Default::default()
        });
        let screen = render_lines(&mut app, 45, 24);
        assert!(screen.contains('…'));
        let (_, area) = app
            .areas
            .controls
            .iter()
            .find(|(c, _)| *c == Control::CycleScope)
            .unwrap();
        assert!(screen.lines().nth(area.y as usize).unwrap().contains(" ]"));
        for width in 1..60 {
            for height in [1, 5, 18, 19, 24] {
                render(&mut app, width, height);
                for (_, area) in &app.areas.controls {
                    assert!(area.right() <= width && area.bottom() <= height);
                    assert!(app.areas.panel_at(area.x, area.y).is_none());
                }
            }
        }
    }

    #[test]
    fn toolbar_only_exposes_execution_actions_supported_for_the_selected_scope() {
        let mut app = App::new("http://localhost".into());
        app.execution_scope_override = true;
        app.sessions.push(v2::Session {
            session_id: "session".into(),
            ..Default::default()
        });
        app.groups.push(v2::Group {
            group_id: "group".into(),
            ..Default::default()
        });
        app.apply_threads(vec![v2::Thread {
            thread_id: "thread".into(),
            session_id: "session".into(),
            ..Default::default()
        }]);
        app.capabilities.execution_actions = vec![
            v2::ExecutionAction::Continue as i32,
            v2::ExecutionAction::Next as i32,
        ];
        app.capabilities.execution_action_capabilities = vec![
            v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Continue as i32,
                scopes: vec![
                    v2::ExecutionScopeKind::Thread as i32,
                    v2::ExecutionScopeKind::Session as i32,
                    v2::ExecutionScopeKind::Group as i32,
                ],
            },
            v2::ExecutionActionCapability {
                action: v2::ExecutionAction::Next as i32,
                scopes: vec![v2::ExecutionScopeKind::Thread as i32],
            },
        ];
        for target in [
            crate::api::session_target("session"),
            crate::api::group_target("group"),
        ] {
            app.execution_target = Some(target);
            render(&mut app, 140, 40);
            assert!(app
                .areas
                .controls
                .iter()
                .any(|(control, _)| *control == Control::Continue));
            assert!(!app
                .areas
                .controls
                .iter()
                .any(|(control, _)| *control == Control::Next));
        }
        app.execution_target = Some(crate::api::thread_target("thread"));
        render(&mut app, 140, 40);
        assert!(app
            .areas
            .controls
            .iter()
            .any(|(control, _)| *control == Control::Next));
        app.execution_target = None;
        app.threads.clear();
        render(&mut app, 140, 40);
        assert!(!app
            .areas
            .controls
            .iter()
            .any(|(control, _)| control.action_name().is_some()));
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
        assert!(last.contains("Esc or ? closes help"));
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
    fn extension_document_cache_survives_identical_snapshots_and_invalidates_on_change() {
        use ddb_api_extension::ExtensionProvider;
        let provider = ddb_sample_extension::SampleWorkersExtension::default();
        let mut app = App::new("http://localhost".into());
        app.apply_capabilities(v2::Capabilities {
            extensions: vec![provider.descriptor()],
            ..Default::default()
        });
        app.focus = Focus::Extensions;
        app.expanded_pane = true;
        let snapshot = v2::Snapshot {
            extension_states: vec![v2::ExtensionState {
                extension_state_id: "workers".into(),
                extension_id: ddb_sample_extension::EXTENSION_ID.into(),
                revision: 1,
                payloads: provider.state().unwrap(),
            }],
            ..Default::default()
        };
        app.apply_snapshot(snapshot.clone());
        assert!(render(&mut app, 140, 40).contains("alpha"));
        assert!(app.extension_document.is_some());
        app.apply_snapshot(snapshot.clone());
        assert!(
            app.extension_document.is_some(),
            "unchanged data must not trigger reflow"
        );
        let mut changed = snapshot.clone();
        changed.extension_states[0].payloads.clear();
        app.apply_snapshot(changed);
        assert!(app.extension_document.is_none());
        assert!(!render(&mut app, 140, 40).contains("Worker: alpha"));
        app.apply_snapshot(snapshot);
        assert!(app.extension_document.is_none());
        assert!(render(&mut app, 140, 40).contains("Worker: alpha"));
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

        app.focus = Focus::Extensions;
        app.move_selection(4);
        assert_eq!(app.extension_scroll, 8);
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("Topology"));

        app.extension_scroll = usize::MAX;
        let screen = render(&mut app, 140, 40);
        assert!(screen.contains("sample provider ready"));
        assert!(screen.contains("move_worker"));
        assert_ne!(app.extension_scroll, usize::MAX);
        let bottom = app.extension_scroll;
        app.move_selection(-1);
        assert_eq!(app.extension_scroll, bottom - 1);
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
    fn timeline_scroll_reaches_the_end_of_one_tall_event() {
        let mut app = App::new("http://localhost".into());
        app.show_timeline = true;
        app.focus = Focus::Timeline;
        app.expanded_pane = true;
        app.push_timeline(format!("BEGIN {} END_MARKER", "word ".repeat(200)));
        render(&mut app, 45, 14);
        app.move_selection(1000);
        assert!(render(&mut app, 45, 14).contains("END_MARKER"));
        app.move_selection(-1000);
        assert!(render(&mut app, 45, 14).contains("BEGIN"));
        assert!(render(&mut app, 45, 14).contains("BEGIN"));
        render(&mut app, 200, 100);
        assert_eq!(app.timeline_scroll, 0);
        for (width, height) in [(1, 1), (10, 3), (45, 14)] {
            render(&mut app, width, height);
        }
    }

    #[test]
    fn timeline_wrapping_uses_display_width_and_word_boundaries() {
        let mut app = App::new("http://localhost".into());
        app.show_timeline = true;
        app.focus = Focus::Timeline;
        app.expanded_pane = true;
        app.push_timeline(format!(
            "BEGIN {} END_MARKER",
            "界界 abcdefghij ".repeat(100)
        ));
        assert!(render(&mut app, 45, 14).contains("END_MARKER"));
        app.move_selection(-1000);
        assert!(render(&mut app, 45, 14).contains("BEGIN"));
        app.move_selection(1000);
        assert!(render(&mut app, 45, 14).contains("END_MARKER"));
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
    fn timeline_tail_survives_more_than_u16_wrapped_history_rows() {
        let mut app = App::new("http://localhost".into());
        app.show_timeline = true;
        app.focus = Focus::Timeline;
        app.expanded_pane = true;
        for _ in 0..499 {
            app.push_timeline("word ".repeat(400));
        }
        app.push_timeline("TAIL");
        assert!(render(&mut app, 15, 14).contains("TAIL"));
    }

    #[test]
    fn extension_cell_beyond_u16_rows_is_fully_reachable_and_reflows_on_resize() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Extensions;
        app.expanded_pane = true;
        app.extension_panels.push(crate::model::ExtensionPanelView {
            extension_title: "Extension".into(),
            panel_title: "Large data".into(),
            description: String::new(),
            columns: vec!["Value".into()],
            rows: vec![vec![format!(
                "BEGIN {} TAIL",
                "界e\u{301} ".repeat(210_000)
            )]],
        });
        app.extension_scroll = usize::MAX;
        assert!(render(&mut app, 15, 14).contains("TAIL"));
        assert!(app.extension_scroll > u16::MAX as usize);
        app.move_selection(isize::MIN);
        assert!(render(&mut app, 15, 14).contains("BEGIN"));
        app.move_selection(isize::MAX);
        assert!(render(&mut app, 15, 14).contains("TAIL"));
        assert!(render(&mut app, 45, 14).contains("TAIL"));
        app.apply_snapshot(v2::Snapshot::default());
        assert!(!render(&mut app, 45, 14).contains("TAIL"));
    }

    #[test]
    fn one_timeline_event_beyond_u16_rows_remains_fully_navigable() {
        let mut app = App::new("http://localhost".into());
        app.show_timeline = true;
        app.focus = Focus::Timeline;
        app.expanded_pane = true;
        app.push_timeline(format!("BEGIN {} TAIL", "word ".repeat(210_000)));
        assert!(render(&mut app, 15, 14).contains("TAIL"));
        app.move_selection(isize::MIN);
        assert!(render(&mut app, 15, 14).contains("BEGIN"));
        assert!(app.timeline_scroll > u16::MAX as usize);
        app.move_selection(isize::MAX);
        assert!(render(&mut app, 15, 14).contains("TAIL"));
        assert!(render(&mut app, 45, 14).contains("TAIL"));
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
        let screen = render_lines(&mut app, 100, 24);
        assert!(screen.contains("[1 hit]"), "{screen}");
        assert!(screen.contains("total hits:15"));
        let breakpoint_row = screen
            .lines()
            .find(|line| line.contains("hit_here"))
            .unwrap();
        assert!(breakpoint_row.find('●').unwrap() < breakpoint_row.find("hit_here").unwrap());
        assert!(breakpoint_row.find("hit_here").unwrap() < breakpoint_row.find("[1 hit]").unwrap());
        assert!(!breakpoint_row.contains("▶ 1 paused"));
        assert!(!breakpoint_row.contains("hit_here:0"));
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
        assert!(!screen.contains("[1 hit]"), "{screen}");
        assert!(!screen.contains("DDB Breakpoints · 1 hit"), "{screen}");
        assert!(screen.contains("total hits:15"));
    }

    #[test]
    fn breakpoint_footer_actions_do_not_cover_the_title_or_each_other() {
        let mut app = App::new("http://localhost".into());
        app.focus = Focus::Breakpoints;
        for width in [140, 160, 45] {
            render(&mut app, width, 40);
            let group = app.areas.breakpoint_grouping.unwrap();
            let paused = app.areas.breakpoint_hit_view_toggle.unwrap();
            let expand = app
                .areas
                .pane_expanders
                .iter()
                .find(|(focus, _)| *focus == Focus::Breakpoints)
                .unwrap()
                .1;
            assert!(group.intersection(paused).is_empty());
            assert!(group.intersection(expand).is_empty());
            let panel = app
                .areas
                .panels
                .iter()
                .find(|panel| panel.focus == Focus::Breakpoints)
                .unwrap();
            assert_eq!(group.y, panel.area.bottom() - 1);
        }
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
            lookup: None,
            loading: false,
            message: None,
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
