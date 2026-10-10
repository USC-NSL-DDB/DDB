use std::ops::Range;

use ratatui::{
    style::Style,
    text::{Line, Span},
};

struct SourceLine {
    text: String,
    spans: Vec<(Range<usize>, Style)>,
}

/// Left-aligned styled text with grapheme-aligned visual rows. Reflow happens
/// only on width changes; scrolling borrows just the visible rows. Offsets never
/// enter Paragraph's u16 scrolling API.
pub struct WrappedText {
    lines: Vec<SourceLine>,
    rows: Vec<(usize, Range<usize>)>,
    width: Option<u16>,
}

impl WrappedText {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        let lines = lines
            .into_iter()
            .map(|line| {
                let mut text = String::new();
                let mut spans = Vec::new();
                for span in line.spans {
                    let start = text.len();
                    text.push_str(&crate::display_text::terminal_text(&span.content));
                    spans.push((start..text.len(), line.style.patch(span.style)));
                }
                SourceLine { text, spans }
            })
            .collect();
        Self {
            lines,
            rows: Vec::new(),
            width: None,
        }
    }

    pub fn visible(&mut self, width: u16, height: u16, top: &mut usize) -> Vec<Line<'_>> {
        if width == 0 || height == 0 {
            return Vec::new();
        }
        if self.width != Some(width) {
            self.rows = self
                .lines
                .iter()
                .enumerate()
                .flat_map(|(index, line)| {
                    wrap_ranges(&line.text, width as usize)
                        .into_iter()
                        .map(move |range| (index, range))
                })
                .collect();
            self.width = Some(width);
        }
        *top = (*top).min(self.rows.len().saturating_sub(height as usize));
        self.rows
            .iter()
            .skip(*top)
            .take(height as usize)
            .map(|(index, row)| {
                let source = &self.lines[*index];
                Line::from(
                    source
                        .spans
                        .iter()
                        .filter_map(|(span, style)| {
                            let start = span.start.max(row.start);
                            let end = span.end.min(row.end);
                            (start < end).then(|| Span::styled(&source.text[start..end], *style))
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }
}

/// Prefer whitespace boundaries, splitting overlong words only at graphemes.
/// A separator overflowing the right edge is consumed at the wrap, not moved
/// onto the next line. Other whitespace is retained. A glyph wider than the pane
/// occupies a row on its own and becomes visible when the pane is widened.
pub(crate) fn wrap_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    let span = Span::raw(text);
    let mut rows = Vec::new();
    let (mut start, mut offset, mut cells) = (0, 0, 0);
    let mut boundary = None;
    for grapheme in span.styled_graphemes(Style::default()) {
        let size = Span::raw(grapheme.symbol).width();
        if cells + size > width && offset > start {
            if grapheme.is_whitespace() {
                rows.push(start..offset);
                offset += grapheme.symbol.len();
                start = offset;
                cells = 0;
                boundary = None;
                continue;
            }
            if let Some((end, used)) = boundary.take() {
                rows.push(start..end);
                start = end;
                cells -= used;
            }
            if cells + size > width && offset > start {
                rows.push(start..offset);
                start = offset;
                cells = 0;
            }
        }
        offset += grapheme.symbol.len();
        cells += size;
        if grapheme.is_whitespace() {
            boundary = Some((offset, cells));
        }
    }
    if start < text.len() || rows.is_empty() {
        rows.push(start..text.len());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn styled_rows_preserve_graphemes_and_reuse_the_wrap_cache() {
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let mut text = WrappedText::new(vec![Line::from(vec![
            Span::styled("界e\u{301} ", red),
            Span::styled("tail", blue),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD))]);
        let mut top = 0;
        let rows = text.visible(4, 2, &mut top);
        assert_eq!(rows[0].to_string(), "界e\u{301} ");
        assert_eq!(rows[1].to_string(), "tail");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(rows[1].spans[0].style.fg, Some(Color::Blue));
        assert!(rows[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        let cached = text.rows.as_ptr();
        top = usize::MAX;
        assert_eq!(text.visible(4, 1, &mut top)[0].to_string(), "tail");
        assert_eq!(text.rows.as_ptr(), cached);
        assert_eq!(
            text.visible(20, 1, &mut top)[0].to_string(),
            "界e\u{301} tail"
        );
        assert_eq!(top, 0);
        assert!(text.visible(0, 0, &mut top).is_empty());
    }

    #[test]
    fn many_logical_rows_cross_u16_without_losing_empty_rows_or_controls() {
        let mut text = WrappedText::new(
            (0..70_000)
                .map(|index| Line::from(format!("{index:05}")))
                .collect(),
        );
        let mut top = 65_535;
        let rows = text.visible(10, 3, &mut top);
        assert_eq!(
            rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["65535", "65536", "65537"]
        );
        top = usize::MAX;
        assert_eq!(text.visible(10, 1, &mut top)[0].to_string(), "69999");
        let mut text = WrappedText::new(vec![Line::from(""), Line::from("x\x1by")]);
        top = 0;
        let rows = text.visible(20, 3, &mut top);
        assert_eq!(rows[0].to_string(), "");
        assert_eq!(rows[1].to_string(), "x�y");
        let mut empty = WrappedText::new(Vec::new());
        assert!(empty.visible(20, 3, &mut top).is_empty());
    }
}
