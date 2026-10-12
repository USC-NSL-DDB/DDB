use ratatui::{
    style::Style,
    text::{Line, Span},
};

#[derive(Clone, Debug, Default)]
pub struct HorizontalViewport {
    column: usize,
    maximum: usize,
}

impl HorizontalViewport {
    pub fn column(&self) -> usize {
        self.column
    }

    pub fn fit(&mut self, content_width: usize, visible_width: usize) {
        self.maximum = content_width.saturating_sub(visible_width.max(1));
        self.column = self.column.min(self.maximum);
    }

    pub fn scroll(&mut self, delta: isize) {
        self.column = self.column.saturating_add_signed(delta).min(self.maximum);
    }
}

/// Clip a styled line without losing colors or splitting terminal graphemes.
pub fn horizontal_line(mut line: Line<'static>, mut column: usize) -> Line<'static> {
    line.spans = line
        .spans
        .into_iter()
        .filter_map(|mut span| {
            let span_width = width(&span.content);
            if column >= span_width {
                column -= span_width;
                return None;
            }
            span.content = horizontal_text(&span.content, column).into();
            column = 0;
            Some(span)
        })
        .collect();
    line
}

pub fn terminal_text(input: &str) -> String {
    input
        .chars()
        .map(|character| match character {
            '\t' => ' ',
            character if character.is_control() => '�',
            character => character,
        })
        .collect()
}

pub fn width(input: &str) -> usize {
    Span::raw(terminal_text(input)).width()
}

/// Keep styles and grapheme clusters intact when reserving cells for a badge
/// or shortcut. An ellipsis makes clipped information explicit.
pub fn truncate_line(line: Line<'static>, maximum: usize) -> Line<'static> {
    if line.width() <= maximum {
        return line;
    }
    if maximum == 0 {
        return Line::default();
    }
    let mut spans = Vec::new();
    let style = line.style;
    let mut used = 0;
    'outer: for span in line.spans {
        let mut text = String::new();
        for grapheme in span.styled_graphemes(Style::default()) {
            let width = Span::raw(grapheme.symbol).width();
            if used + width > maximum - 1 {
                spans.push(Span::styled(text, span.style));
                break 'outer;
            }
            used += width;
            text.push_str(grapheme.symbol);
        }
        spans.push(Span::styled(text, span.style));
    }
    spans.push(Span::raw("…"));
    Line::from(spans).style(style)
}

/// Clip by terminal cells, preserving grapheme clusters. A wide glyph bisected
/// by the viewport edge becomes padding instead of shifting the following text.
pub fn horizontal_text(input: &str, column: usize) -> String {
    if column == 0 {
        return terminal_text(input);
    }
    let span = Span::raw(terminal_text(input));
    let mut position = 0;
    let mut output = String::new();
    for grapheme in span.styled_graphemes(Style::default()) {
        let end = position + Span::raw(grapheme.symbol).width();
        if position >= column {
            output.push_str(grapheme.symbol);
        } else if end > column {
            output.push_str(&" ".repeat(end - column));
        }
        position = end;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_line_clipping_and_viewport_limits() {
        let red = Style::default().fg(ratatui::style::Color::Red);
        let blue = Style::default().fg(ratatui::style::Color::Blue);
        let line = Line::from(vec![Span::styled("界", red), Span::styled("abc", blue)]);
        let clipped = horizontal_line(line.clone(), 1);
        assert_eq!(clipped.spans[0].content, " ");
        assert_eq!(clipped.spans[0].style, red);
        assert_eq!(clipped.spans[1].style, blue);
        assert_eq!(horizontal_line(line, 3).spans[0].content, "bc");
        let mut viewport = HorizontalViewport::default();
        viewport.fit(100, 20);
        viewport.scroll(isize::MAX);
        assert_eq!(viewport.column(), 80);
        viewport.fit(100, 95);
        assert_eq!(viewport.column(), 5);
        viewport.scroll(isize::MIN);
        assert_eq!(viewport.column(), 0);
        viewport.fit(0, 0);
        viewport.scroll(8);
        assert_eq!(viewport.column(), 0);
    }

    #[test]
    fn horizontal_clipping_preserves_unicode_and_sanitizes_controls() {
        assert_eq!(width("A界e\u{301}Z"), 5);
        assert_eq!(horizontal_text("A界e\u{301}Z", 1), "界e\u{301}Z");
        assert_eq!(horizontal_text("A界e\u{301}Z", 2), " e\u{301}Z");
        assert_eq!(horizontal_text("A界e\u{301}Z", 3), "e\u{301}Z");
        assert_eq!(horizontal_text("abc", 100), "");
        assert_eq!(horizontal_text("\t\x1btest", 0), " �test");
    }

    #[test]
    fn truncation_preserves_styles_graphemes_and_cell_bounds() {
        let style = Style::default().fg(ratatui::style::Color::Red);
        let line = Line::from(vec![Span::styled("界e\u{301}AB", style)]);
        assert_eq!(truncate_line(line.clone(), 4).to_string(), "界e\u{301}…");
        assert_eq!(truncate_line(line.clone(), 2).to_string(), "…");
        assert_eq!(truncate_line(line.clone(), 1).to_string(), "…");
        assert_eq!(truncate_line(line.clone(), 0).width(), 0);
        assert_eq!(truncate_line(line.clone(), 20), line);
        assert_eq!(truncate_line(line, 4).spans[0].style, style);
    }
}
