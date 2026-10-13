use super::*;

struct Button {
    control: Control,
    label: Line<'static>,
    area: Rect,
}

/// Measurement and hit testing use the same complete, terminal-cell-sized
/// buttons. Execution and navigation share a row when both groups fit.
fn layout(app: &App, width: u16) -> (Vec<Button>, u16) {
    let mut buttons = Vec::new();
    let mut row = 0;
    let mut x: u16 = 0;
    let groups: [Vec<(Control, &str, String)>; 2] = [
        vec![
            (Control::Continue, "▶", "Continue".into()),
            (Control::Interrupt, "⏸", "Pause".into()),
            (Control::Next, "↷", "Step over".into()),
            (Control::StepIn, "↓", "Step in".into()),
            (Control::StepOut, "↑", "Step out".into()),
        ],
        vec![
            (Control::RevealFrame, "", "Frame source".into()),
            (
                Control::RefreshStack,
                "↻",
                if app.stack_mode == crate::cli::StackMode::Local {
                    "Local stack"
                } else {
                    "DDB stack"
                }
                .into(),
            ),
            (Control::Refresh, "", "Refresh".into()),
        ],
    ];
    for group in groups {
        let mut group_buttons = Vec::new();
        for (control, icon, name) in group {
            if !available(app, control) {
                continue;
            }
            let color = match control {
                Control::Continue => GREEN,
                Control::Interrupt => YELLOW,
                Control::RevealFrame
                | Control::RefreshStack
                | Control::Refresh
                | Control::CycleScope => ACCENT,
                _ => Color::White,
            };
            let pending = control
                .action_name()
                .and_then(|action| app.control_target(action))
                .is_some_and(|target| app.operations.control_pending(control, &target));
            let icon = if pending {
                format!("{} ", app.operations.spinner())
            } else if icon.is_empty() {
                String::new()
            } else {
                format!("{icon} ")
            };
            let name = terminal_text(&name);
            let name = if control == Control::CycleScope {
                let space = usize::from(width).saturating_sub(4);
                crate::display_text::truncate_line(Line::raw(name), space).to_string()
            } else {
                name
            };
            let label = Line::from(vec![
                Span::raw("[ "),
                Span::styled(format!("{icon}{name}"), Style::default().fg(color)),
                Span::raw(" ]"),
            ]);
            let button_width = u16::try_from(label.width()).unwrap_or(u16::MAX);
            // Do not create a clickable fragment of a static action label.
            if button_width > width || button_width == 0 {
                continue;
            }
            group_buttons.push(Button {
                control,
                label,
                area: Rect::new(0, 0, button_width, 1),
            });
        }
        if group_buttons.is_empty() {
            continue;
        }
        let group_width = group_buttons
            .iter()
            .map(|button| u32::from(button.area.width))
            .sum::<u32>()
            + group_buttons.len().saturating_sub(1) as u32;
        if x > 0 {
            if u32::from(x) + 2 + group_width > u32::from(width) {
                row += 1;
                x = 0;
            } else {
                x += 2; // Larger gap between execution and navigation.
            }
        }
        for mut button in group_buttons {
            if x > 0 && x.saturating_add(button.area.width) > width {
                row += 1;
                x = 0;
            }
            button.area.x = x;
            button.area.y = row;
            x = x.saturating_add(button.area.width).saturating_add(1);
            buttons.push(button);
        }
    }
    let height = buttons.last().map_or(1, |button| button.area.y + 1);
    (buttons, height)
}

fn available(app: &App, control: Control) -> bool {
    match control {
        Control::RefreshStack => {
            app.stack_mode == crate::cli::StackMode::Local
                || app
                    .capabilities
                    .supports_ddb_feature("distributed_backtrace")
        }
        Control::CycleScope | Control::Refresh | Control::RevealFrame => true,
        control => control.action_name().is_some_and(|action| {
            app.control_target(action)
                .as_ref()
                .is_some_and(|target| app.capabilities.supports_execution_target(action, target))
        }),
    }
}

pub(super) fn height(app: &App, width: u16) -> u16 {
    layout(app, width.saturating_sub(2)).1.saturating_add(2)
}

pub(super) fn draw(frame: &mut Frame<'_>, app: &App, area: Rect, areas: &mut UiAreas) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " DDB TU · ? help ",
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let reserved_title = 22;
    let scope_width = area.width.saturating_sub(reserved_title);
    if scope_width >= 17 && area.height > 0 {
        let label = crate::display_text::truncate_line(
            Line::raw(terminal_text(&app.control_scope_label())),
            usize::from(scope_width).saturating_sub(14),
        );
        let scope = Line::raw(format!(" Scope: {} [c] ", label));
        let rect = Rect::new(
            area.right() - scope.width() as u16 - 1,
            area.y,
            scope.width() as u16,
            1,
        );
        frame.render_widget(
            Paragraph::new(scope).style(Style::default().fg(ACCENT)),
            rect,
        );
        areas.controls.push((Control::CycleScope, rect));
    }
    if inner.is_empty() {
        return;
    }
    let (buttons, _) = layout(app, inner.width);
    for button in buttons {
        if button.area.y >= inner.height {
            continue;
        }
        let rect = Rect::new(
            inner.x + button.area.x,
            inner.y + button.area.y,
            button.area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(button.label).style(Style::default().bg(Color::Rgb(35, 42, 55))),
            rect,
        );
        areas.controls.push((button.control, rect));
    }
}

pub(super) fn connection_summary(app: &App) -> Line<'static> {
    let connection = if app.api_connected {
        Span::styled("● connected", Style::default().fg(GREEN))
    } else {
        Span::styled("● reconnecting", Style::default().fg(RED))
    };
    let detail = if app.trace_api {
        format!(
            " · {} · {} · events:{} · jobs:{}",
            endpoint_label(&app.api_endpoint),
            app.api_protocol,
            if app.event_stream_connected {
                "up"
            } else {
                "retry"
            },
            app.pending_commands
        )
    } else {
        format!(
            " · {} running · {} stopped",
            app.threads
                .iter()
                .filter(|thread| thread.state == "running")
                .count(),
            app.threads
                .iter()
                .filter(|thread| thread.state == "stopped")
                .count()
        )
    };
    Line::from(vec![
        connection,
        Span::styled(terminal_text(&detail), Style::default().fg(MUTED)),
    ])
}
