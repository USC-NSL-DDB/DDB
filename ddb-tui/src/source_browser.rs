//! Document navigation, independent of debugger selection and inspection.

use std::collections::VecDeque;

use ddb_api_client::v2;

#[cfg(test)]
#[path = "source_browser_tests.rs"]
pub(crate) mod tests;

const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct SourceDocument {
    pub source: v2::SourceFile,
    pub target: Option<v2::Target>,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: Option<usize>,
    pub lines: Vec<String>,
    pub cursor: usize,
    pub scroll: usize,
}

#[derive(Clone, Debug)]
pub struct SourceRequest {
    pub generation: u64,
    pub target: Option<v2::Target>,
    pub location: v2::SourceLocation,
    pub line: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Navigation {
    Manual,
    Frame,
    Follow,
}

#[derive(Clone, Debug)]
struct Position {
    target: Option<v2::Target>,
    location: v2::SourceLocation,
    scroll: usize,
}

#[derive(Debug)]
pub struct SourceBrowser {
    document: Option<SourceDocument>,
    following: bool,
    generation: u64,
    pending: Option<SourceRequest>,
    back: VecDeque<Position>,
    forward: VecDeque<Position>,
    restore_scroll: Option<usize>,
    record_history: bool,
    history_direction: Option<bool>,
}

impl Default for SourceBrowser {
    fn default() -> Self {
        Self {
            document: None,
            following: true,
            generation: 0,
            pending: None,
            back: VecDeque::new(),
            forward: VecDeque::new(),
            restore_scroll: None,
            record_history: false,
            history_direction: None,
        }
    }
}

impl SourceBrowser {
    pub fn document(&self) -> Option<&SourceDocument> {
        self.document.as_ref()
    }

    pub fn following(&self) -> bool {
        self.following
    }

    pub fn set_following(&mut self, following: bool) {
        self.following = following;
        self.cancel_pending();
    }

    pub fn pending_generation(&self) -> Option<u64> {
        self.pending.as_ref().map(|request| request.generation)
    }

    pub fn navigate(
        &mut self,
        target: Option<v2::Target>,
        mut location: v2::SourceLocation,
        navigation: Navigation,
    ) -> Option<SourceRequest> {
        if navigation == Navigation::Follow && !self.following {
            return None;
        }
        if navigation != Navigation::Follow {
            self.following = false;
        }
        location.line = location.line.max(1);
        self.record_history = navigation != Navigation::Follow;
        self.history_direction = None;
        self.restore_scroll = None;
        Some(self.request(target, location))
    }

    pub fn history(&mut self, forward: bool) -> Option<SourceRequest> {
        let destination = if forward {
            self.forward.back()?.clone()
        } else {
            self.back.back()?.clone()
        };
        self.following = false;
        self.record_history = false;
        self.history_direction = Some(forward);
        self.restore_scroll = Some(destination.scroll);
        Some(self.request(destination.target, destination.location))
    }

    pub fn apply(&mut self, generation: u64, content: v2::SourceContent) -> bool {
        if self.pending_generation() != Some(generation) {
            return false;
        }
        let Some(source) = content.source else {
            self.pending = None;
            return false;
        };
        let request = self.pending.take().expect("generation checked above");
        if let Some(forward) = self.history_direction.take() {
            let current = self.position();
            if forward {
                self.forward.pop_back();
                if let Some(current) = current {
                    push_bounded(&mut self.back, current);
                }
            } else {
                self.back.pop_back();
                if let Some(current) = current {
                    push_bounded(&mut self.forward, current);
                }
            }
        } else if self.record_history {
            if let Some(current) = self.position() {
                if current.target != request.target
                    || !same_document(&current.location, &request.location)
                    || current.location.line != request.location.line
                {
                    push_bounded(&mut self.back, current);
                    self.forward.clear();
                }
            }
        }
        let first = (content.start_line as usize).max(1);
        let mut lines = content
            .content
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        lines.truncate(content.line_count as usize);
        let last = first.saturating_add(lines.len().saturating_sub(1));
        let cursor = request.line.clamp(first, last);
        self.document = Some(SourceDocument {
            path: source.path.clone().unwrap_or_else(|| source.name.clone()),
            source,
            target: request.target,
            start_line: first,
            end_line: last,
            total_lines: (!content.has_more).then_some(last),
            lines,
            cursor,
            scroll: self
                .restore_scroll
                .take()
                .unwrap_or_else(|| cursor.saturating_sub(first).saturating_sub(8)),
        });
        true
    }

    pub fn fail(&mut self, generation: u64) -> bool {
        if self.pending_generation() != Some(generation) {
            return false;
        }
        self.pending = None;
        self.restore_scroll = None;
        self.history_direction = None;
        true
    }

    pub fn cursor_location(&self) -> Option<(&str, usize)> {
        self.document
            .as_ref()
            .map(|document| (document.path.as_str(), document.cursor))
    }

    pub fn set_cursor(&mut self, line: usize) {
        self.following = false;
        self.cancel_pending();
        if let Some(document) = &mut self.document {
            document.cursor = line.clamp(document.start_line, document.end_line);
            document.scroll = document
                .cursor
                .saturating_sub(document.start_line)
                .saturating_sub(8);
        }
    }

    pub fn current_location(
        &self,
        line: usize,
    ) -> Option<(Option<v2::Target>, v2::SourceLocation)> {
        let document = self.document.as_ref()?;
        Some((
            document.target.clone(),
            v2::SourceLocation {
                source_reference: Some(document.source.source_reference.clone()),
                path: Some(document.path.clone()),
                line: u32::try_from(line.max(1)).ok()?,
                ..Default::default()
            },
        ))
    }

    fn position(&self) -> Option<Position> {
        let document = self.document.as_ref()?;
        let (target, location) = self.current_location(document.cursor)?;
        Some(Position {
            target,
            location,
            scroll: document.scroll,
        })
    }

    fn request(
        &mut self,
        target: Option<v2::Target>,
        location: v2::SourceLocation,
    ) -> SourceRequest {
        self.generation = self.generation.wrapping_add(1).max(1);
        let request = SourceRequest {
            generation: self.generation,
            line: location.line as usize,
            target,
            location,
        };
        self.pending = Some(request.clone());
        request
    }

    fn cancel_pending(&mut self) {
        self.pending = None;
        self.restore_scroll = None;
        self.history_direction = None;
    }
}

fn same_document(left: &v2::SourceLocation, right: &v2::SourceLocation) -> bool {
    match (&left.source_reference, &right.source_reference) {
        (Some(left), Some(right)) => left == right,
        _ => left.path.is_some() && left.path == right.path,
    }
}

fn push_bounded(history: &mut VecDeque<Position>, position: Position) {
    if history.len() == HISTORY_LIMIT {
        history.pop_front();
    }
    history.push_back(position);
}
