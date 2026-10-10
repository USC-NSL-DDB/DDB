use std::{collections::VecDeque, ops::Range};

use crate::wrapped_text::wrap_ranges as wrap;

/// Retained events and cached, grapheme-aligned visual rows. Row offsets never
/// enter Paragraph's u16 scroll API. Only visible rows are handed to rendering.
#[derive(Default)]
pub struct Timeline {
    events: VecDeque<Event>,
    width: Option<usize>,
}

struct Event {
    text: String,
    rows: Vec<Range<usize>>,
}

impl Timeline {
    pub fn push(&mut self, text: String) {
        if self.events.len() == 500 {
            self.events.pop_front();
        }
        let text = crate::display_text::terminal_text(&text);
        let rows = self
            .width
            .map(|width| wrap(&text, width))
            .unwrap_or_default();
        self.events.push_back(Event { text, rows });
    }

    pub fn visible(&mut self, width: u16, height: u16, above_tail: &mut usize) -> Vec<&str> {
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let width = usize::from(width);
        if self.width != Some(width) {
            for event in &mut self.events {
                event.rows = wrap(&event.text, width);
            }
            self.width = Some(width);
        }
        let total = self
            .events
            .iter()
            .map(|event| event.rows.len())
            .sum::<usize>();
        let maximum = total.saturating_sub(height as usize);
        *above_tail = (*above_tail).min(maximum);
        let mut skip = maximum - *above_tail;
        let mut visible = Vec::with_capacity(height as usize);
        for event in &self.events {
            if skip >= event.rows.len() {
                skip -= event.rows.len();
                continue;
            }
            for range in event
                .rows
                .iter()
                .skip(skip)
                .take(height as usize - visible.len())
            {
                visible.push(&event.text[range.clone()]);
            }
            skip = 0;
            if visible.len() == height as usize {
                break;
            }
        }
        visible
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.events.len()
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    #[cfg(test)]
    pub fn back(&self) -> Option<&String> {
        self.events.back().map(|event| &event.text)
    }
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.events.iter().map(|event| &event.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{style::Style, text::Span};

    #[test]
    fn wrapping_preserves_content_and_graphemes() {
        for text in [
            "",
            "aa bb cc",
            "   abc   def ",
            "abcdefghij",
            "界e\u{301} 👩‍💻 world",
            "a\u{a0}b",
        ] {
            for width in 1..16 {
                let rows = wrap(text, width);
                let joined = rows
                    .iter()
                    .map(|range| &text[range.clone()])
                    .collect::<String>();
                assert_eq!(
                    joined
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>(),
                    text.chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                );
                for range in rows {
                    let row = &text[range];
                    let span = Span::raw(row);
                    assert!(
                        span.width() <= width
                            || span.styled_graphemes(Style::default()).count() == 1
                    );
                }
            }
        }
        assert_eq!(wrap("aa bb cc", 5), vec![0..5, 6..8]);
    }

    #[test]
    fn cache_reuses_rows_and_reflows_on_resize_without_losing_retention() {
        let mut timeline = Timeline::default();
        timeline.push("a b c d e f".into());
        let mut scroll = 0;
        assert_eq!(timeline.visible(3, 2, &mut scroll), vec!["c d", "e f"]);
        let cached = timeline.events[0].rows.as_ptr();
        scroll = usize::MAX;
        assert_eq!(timeline.visible(3, 2, &mut scroll), vec!["a b", "c d"]);
        assert_eq!(timeline.events[0].rows.as_ptr(), cached);
        assert_eq!(timeline.visible(30, 2, &mut scroll), vec!["a b c d e f"]);
        assert_eq!(scroll, 0);
        for i in 0..501 {
            timeline.push(format!("event {i}"));
        }
        assert_eq!(timeline.len(), 500);
        assert_eq!(
            timeline.visible(30, 2, &mut scroll),
            vec!["event 499", "event 500"]
        );
        assert!(timeline.visible(0, 0, &mut scroll).is_empty());
    }

    #[test]
    fn indexed_rows_beyond_u16_are_reachable_inside_one_event() {
        let mut timeline = Timeline::default();
        timeline.push((0..70_000).map(|index| format!("{index:05} ")).collect());
        let mut scroll = 0;
        assert_eq!(
            timeline.visible(6, 3, &mut scroll),
            ["69997 ", "69998 ", "69999 "]
        );
        scroll = 69_997 - 65_536;
        assert_eq!(
            timeline.visible(6, 3, &mut scroll),
            ["65536 ", "65537 ", "65538 "]
        );
        scroll += 1;
        assert_eq!(timeline.visible(6, 3, &mut scroll)[0], "65535 ");
        scroll = usize::MAX;
        assert_eq!(
            timeline.visible(6, 3, &mut scroll),
            ["00000 ", "00001 ", "00002 "]
        );
    }
}
