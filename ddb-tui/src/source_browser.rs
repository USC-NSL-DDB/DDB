//! Transactional document navigation, independent of debugger selection.

use crate::source_types::{Address, Content, Root, Source};
use ddb_api_client::v2;
use std::collections::VecDeque;

#[cfg(test)]
#[path = "source_browser_tests.rs"]
pub(crate) mod tests;
const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct SourceDocument {
    pub source: Source,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub lines: Vec<String>,
    pub cursor: usize,
    pub scroll: usize,
    pub column: usize,
    max_width: usize,
}

impl SourceDocument {
    pub fn debugger_binding<'a>(
        &'a self,
        sessions: &'a [v2::Session],
    ) -> Option<crate::source_binding::DebuggerBinding<'a>> {
        crate::source_binding::DebuggerBinding::from_source(&self.source, sessions)
    }
}

#[derive(Clone, Debug)]
pub struct SourceRequest {
    pub generation: u64,
    pub address: Address,
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
    source: Source,
    line: usize,
    scroll: usize,
    column: usize,
}

#[derive(Debug)]
pub struct SourceBrowser {
    error: Option<String>,
    document: Option<SourceDocument>,
    server_instance: Option<String>,
    following: bool,
    generation: u64,
    pending: Option<SourceRequest>,
    back: VecDeque<Position>,
    forward: VecDeque<Position>,
    positions: VecDeque<Position>,
    restore_scroll: Option<usize>,
    restore_column: Option<usize>,
    viewport_width: usize,
    viewport_height: usize,
    scroll_to_bottom: Option<bool>,
    record_history: bool,
    history_direction: Option<bool>,
}

impl Default for SourceBrowser {
    fn default() -> Self {
        Self {
            error: None,
            document: None,
            server_instance: None,
            following: true,
            generation: 0,
            pending: None,
            back: VecDeque::new(),
            forward: VecDeque::new(),
            positions: VecDeque::new(),
            restore_scroll: None,
            restore_column: None,
            viewport_width: 1,
            viewport_height: 1,
            scroll_to_bottom: None,
            record_history: false,
            history_direction: None,
        }
    }
}

impl SourceBrowser {
    pub fn set_server_instance(&mut self, instance: String) {
        if self
            .server_instance
            .as_ref()
            .is_some_and(|previous| previous != &instance)
        {
            self.invalidate_server();
        }
        self.server_instance = Some(instance);
    }

    /// Local files and local history outlive DDB. Remote handles and ephemeral
    /// frame-owner hints do not. Generation counters are never reset.
    pub fn invalidate_server(&mut self) {
        if self
            .document
            .as_ref()
            .is_some_and(|document| !document.source.is_local())
        {
            self.document = None;
        }
        if let Some(document) = &mut self.document {
            document.source.invalidate_server();
        }
        for history in [&mut self.back, &mut self.forward, &mut self.positions] {
            history.retain(|position| position.source.is_local());
            for position in history {
                position.source.invalidate_server();
            }
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|request| !request.address.is_local())
        {
            self.cancel_pending();
        }
    }

    pub fn document(&self) -> Option<&SourceDocument> {
        if self.error.is_some() {
            None
        } else {
            self.document.as_ref()
        }
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn unavailable(&mut self, error: String) {
        self.cancel_pending();
        self.error = Some(error);
    }
    pub fn fail_with_error(&mut self, generation: u64, error: String) -> bool {
        if !self.fail(generation) {
            return false;
        }
        self.error = Some(error);
        true
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

    pub fn revision(&self) -> u64 {
        self.generation
    }

    pub fn navigate(
        &mut self,
        target: Option<v2::Target>,
        location: v2::SourceLocation,
        navigation: Navigation,
    ) -> Option<SourceRequest> {
        let line = location.line.max(1) as usize;
        self.navigate_address(
            Address::Resolve {
                instance: self.server_instance.clone(),
                target,
                location,
            },
            line,
            navigation,
        )
    }

    fn navigate_address(
        &mut self,
        address: Address,
        line: usize,
        navigation: Navigation,
    ) -> Option<SourceRequest> {
        if navigation == Navigation::Follow && !self.following {
            return None;
        }
        if navigation != Navigation::Follow {
            self.following = false;
        }
        self.record_history = navigation != Navigation::Follow;
        self.history_direction = None;
        self.restore_scroll = None;
        self.restore_column = None;
        self.scroll_to_bottom = None;
        Some(self.request(address, line.max(1)))
    }

    pub fn open_export(&mut self, root: Root, path: String) -> SourceRequest {
        self.navigate_address(Address::Export { root, path }, 1, Navigation::Manual)
            .expect("manual navigation is accepted")
    }

    pub fn open_source(&mut self, source: Source) -> SourceRequest {
        let current = self.position();
        let saved = current
            .as_ref()
            .into_iter()
            .chain(self.positions.iter().rev())
            .find(|position| position.source.id() == source.id());
        let line = saved.map_or(1, |position| position.line);
        let scroll = saved.map(|position| position.scroll);
        let column = saved.map(|position| position.column);
        let request = self
            .navigate_address(Address::File(source), line, Navigation::Manual)
            .expect("manual navigation is accepted");
        self.restore_scroll = scroll;
        self.restore_column = column;
        request
    }

    pub fn go_to_line(&mut self, line: usize) -> Option<SourceRequest> {
        let source = self.document.as_ref()?.source.clone();
        self.navigate_address(Address::File(source), line, Navigation::Manual)
    }

    /// Repeated cursor keys accumulate against an in-flight window destination.
    pub fn move_cursor(&mut self, delta: isize) -> Option<SourceRequest> {
        let document = self.document()?;
        let mut line = document.cursor;
        if !self.following && !self.record_history && self.history_direction.is_none() {
            if let Some(pending) = &self.pending {
                if pending.address.id() == Some(document.source.id()) {
                    line = pending.line;
                }
            }
        }
        let line = line
            .saturating_add_signed(delta)
            .clamp(1, document.total_lines.max(1));
        if (document.start_line..=document.end_line).contains(&line) {
            self.set_cursor(line);
            return None;
        }
        let address = Address::File(document.source.clone());
        let scroll_to_bottom = line > document.cursor;
        self.following = false;
        self.record_history = false;
        self.history_direction = None;
        self.restore_scroll = None;
        self.restore_column = None;
        self.scroll_to_bottom = Some(scroll_to_bottom);
        Some(self.request(address, line))
    }

    pub fn history(&mut self, forward: bool) -> Option<SourceRequest> {
        let destination = if forward {
            self.forward.back()?
        } else {
            self.back.back()?
        }
        .clone();
        self.following = false;
        self.record_history = false;
        self.history_direction = Some(forward);
        self.restore_scroll = Some(destination.scroll);
        self.restore_column = Some(destination.column);
        self.scroll_to_bottom = None;
        Some(self.request(Address::File(destination.source), destination.line))
    }

    pub fn apply(&mut self, generation: u64, mut content: Content) -> bool {
        if self.pending_generation() != Some(generation) {
            return false;
        }
        if let Source::Ddb { instance, .. } = &content.source {
            if self
                .server_instance
                .as_ref()
                .is_some_and(|current| current != instance)
            {
                self.cancel_pending();
                return false;
            }
            self.server_instance.get_or_insert_with(|| instance.clone());
        }
        if let Source::Local(source) = &content.source {
            if self.server_instance.is_none() {
                self.server_instance.clone_from(&source.owner_instance);
            }
            if source.owner_instance != self.server_instance {
                content.source.invalidate_server();
            }
        }
        let request = self.pending.take().expect("generation checked");
        if let Some(current) = self.position() {
            self.positions
                .retain(|position| position.source.id() != current.source.id());
            push_bounded(&mut self.positions, current);
        }
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
                if current.source.id() != content.source.id() || current.line != request.line {
                    push_bounded(&mut self.back, current);
                    self.forward.clear();
                }
            }
        }
        let first = if content.total_lines == 0 {
            1
        } else {
            (content.start_line as usize).max(1)
        };
        let mut lines = content
            .content
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        lines.truncate(content.line_count as usize);
        let last = first.saturating_add(lines.len().saturating_sub(1));
        let cursor = request.line.clamp(first, last);
        let column = self.restore_column.take().unwrap_or_else(|| {
            self.document
                .as_ref()
                .filter(|document| document.source.id() == content.source.id())
                .map_or(0, |document| document.column)
        });
        let max_width = lines
            .iter()
            .map(|line| crate::display_text::width(line))
            .max()
            .unwrap_or(0);
        let screen_row = self.scroll_to_bottom.take().map_or(8, |bottom| {
            if bottom {
                self.viewport_height.saturating_sub(1)
            } else {
                0
            }
        });
        self.document = Some(SourceDocument {
            path: content.source.path().into_owned(),
            source: content.source,
            start_line: first,
            end_line: last,
            total_lines: content.total_lines as usize,
            lines,
            cursor,
            column,
            max_width,
            scroll: self
                .restore_scroll
                .take()
                .unwrap_or_else(|| cursor.saturating_sub(first).saturating_sub(screen_row)),
        });
        true
    }

    pub fn fail(&mut self, generation: u64) -> bool {
        if self.pending_generation() != Some(generation) {
            return false;
        }
        self.cancel_pending();
        true
    }
    #[cfg(test)]
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
        }
    }

    /// Keep a stable viewport until the cursor crosses a visible edge.
    /// The renderer supplies the actual content height, including after resize.
    pub fn fit_viewport(&mut self, height: usize) {
        if height > 0 {
            self.viewport_height = height;
        }
        let Some(document) = &mut self.document else {
            return;
        };
        if height == 0 {
            return;
        }
        let cursor = document.cursor.saturating_sub(document.start_line);
        if cursor < document.scroll {
            document.scroll = cursor;
        } else if cursor >= document.scroll.saturating_add(height) {
            document.scroll = cursor.saturating_add(1).saturating_sub(height);
        }
        document.scroll = document
            .scroll
            .min(document.lines.len().saturating_sub(height));
    }
    fn position(&self) -> Option<Position> {
        let document = self.document.as_ref()?;
        Some(Position {
            source: document.source.clone(),
            line: document.cursor,
            scroll: document.scroll,
            column: document.column,
        })
    }
    fn request(&mut self, address: Address, line: usize) -> SourceRequest {
        self.error = None;
        self.generation = self.generation.wrapping_add(1).max(1);
        let request = SourceRequest {
            generation: self.generation,
            address,
            line,
        };
        self.pending = Some(request.clone());
        request
    }
    fn cancel_pending(&mut self) {
        self.pending = None;
        self.restore_scroll = None;
        self.restore_column = None;
        self.scroll_to_bottom = None;
        self.history_direction = None;
    }

    pub fn fit_horizontal(&mut self, width: usize) {
        if width == 0 {
            return;
        }
        self.viewport_width = width;
        if let Some(document) = &mut self.document {
            document.column = document
                .column
                .min(document.max_width.saturating_sub(width));
        }
    }

    pub fn scroll_horizontal(&mut self, delta: isize) {
        if self.error.is_some() {
            return;
        }
        if let Some(document) = &mut self.document {
            document.column = document
                .column
                .saturating_add_signed(delta)
                .min(document.max_width.saturating_sub(self.viewport_width));
        }
    }
}

fn push_bounded(history: &mut VecDeque<Position>, position: Position) {
    if history.len() == HISTORY_LIMIT {
        history.pop_front();
    }
    history.push_back(position);
}
