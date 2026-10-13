use std::collections::VecDeque;

use crate::display_text::{terminal_text, width, HorizontalViewport};

const MAX_LINES: usize = 1000;
const MAX_LINE_CHARS: usize = 4096;

pub struct OutputBuffer {
    lines: VecDeque<(String, usize)>,
    top: usize,
    height: usize,
    following: bool,
    horizontal: HorizontalViewport,
    pub dropped: usize,
}

impl Default for OutputBuffer {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            top: 0,
            height: 1,
            following: true,
            horizontal: Default::default(),
            dropped: 0,
        }
    }
}

impl OutputBuffer {
    pub fn push(&mut self, text: &str) {
        for line in text.split_terminator('\n') {
            let mut chars = line.chars();
            let mut line = terminal_text(&chars.by_ref().take(MAX_LINE_CHARS).collect::<String>());
            if chars.next().is_some() {
                line.push_str(" …[line truncated]");
            }
            let cells = width(&line);
            if self.lines.len() == MAX_LINES {
                self.lines.pop_front();
                self.top = self.top.saturating_sub(1);
                self.dropped = self.dropped.saturating_add(1);
            }
            self.lines.push_back((line, cells));
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        let maximum = self.lines.len().saturating_sub(self.height);
        self.top = if self.following {
            maximum
        } else {
            self.top.min(maximum)
        };
    }

    pub fn fit(&mut self, width: usize, height: usize) {
        self.height = height.max(1);
        self.clamp();
        self.horizontal.fit(
            self.lines
                .iter()
                .map(|(_, width)| *width)
                .max()
                .unwrap_or(0),
            width,
        );
    }

    pub fn scroll(&mut self, delta: isize) {
        let maximum = self.lines.len().saturating_sub(self.height);
        self.top = self.top.saturating_add_signed(delta).min(maximum);
        self.following = delta > 0 && self.top == maximum;
    }

    pub fn follow(&mut self) {
        self.following = true;
        self.clamp();
    }
    pub fn following(&self) -> bool {
        self.following
    }
    pub fn scroll_horizontal(&mut self, delta: isize) {
        self.horizontal.scroll(delta);
    }
    pub fn column(&self) -> usize {
        self.horizontal.column()
    }
    pub fn visible_lines(&self) -> impl Iterator<Item = &str> {
        self.lines
            .iter()
            .skip(self.top)
            .take(self.height)
            .map(|(text, _)| text.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_preserves_history_position_until_follow_is_requested() {
        let mut output = OutputBuffer::default();
        output.fit(20, 2);
        output.push("one\ntwo\nthree\n");
        assert_eq!(output.visible_lines().collect::<Vec<_>>(), ["two", "three"]);
        output.scroll(-1);
        output.push("four\n");
        assert_eq!(output.visible_lines().collect::<Vec<_>>(), ["one", "two"]);
        assert!(!output.following());
        output.follow();
        assert_eq!(
            output.visible_lines().collect::<Vec<_>>(),
            ["three", "four"]
        );
    }

    #[test]
    fn retention_and_line_limits_are_visible_and_unicode_safe() {
        let mut output = OutputBuffer::default();
        for _ in 0..MAX_LINES + 5 {
            output.push("line");
        }
        assert_eq!(output.lines.len(), MAX_LINES);
        assert_eq!(output.dropped, 5);
        output.push(&format!("\x1b{}", "界".repeat(MAX_LINE_CHARS + 1)));
        assert!(output
            .visible_lines()
            .last()
            .unwrap()
            .ends_with("[line truncated]"));
        assert!(!output.visible_lines().last().unwrap().contains('\x1b'));
        output.fit(20, 2);
        output.scroll_horizontal(isize::MAX);
        assert!(output.column() > 0);
        output.fit(20000, 2);
        assert_eq!(output.column(), 0);
    }
}
