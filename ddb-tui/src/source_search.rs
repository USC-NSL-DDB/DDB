//! Search intent, freshness checks, and application of pinned source results.

use crate::{
    source_browser::SourceBrowser,
    source_types::{Content, Source, SourceId},
};
use clap::ValueEnum;
use ddb_source_workspace::{SearchMode, SearchQuery};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum Mode {
    #[default]
    Exact,
    Fuzzy,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fuzzy => "fuzzy",
        }
    }
    pub fn matching(self) -> SearchMode {
        match self {
            Self::Exact => SearchMode::Exact,
            Self::Fuzzy => SearchMode::Fuzzy,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub generation: u64,
    pub source: Source,
    pub cursor: usize,
    pub browser_revision: u64,
    pub query: String,
    pub mode: Mode,
    pub backward: bool,
    pub include_origin: bool,
}

#[derive(Debug)]
pub struct Outcome {
    pub found: Option<ddb_source_workspace::SearchMatch>,
    pub content: Content,
}

struct Highlight {
    source: SourceId,
    line: usize,
    start: usize,
    end: usize,
    text: String,
}

#[derive(Default)]
pub struct Search {
    pub mode: Mode,
    pub query: String,
    next_generation: u64,
    pending: Option<Request>,
    feedback: Option<(SourceId, String)>,
    highlight: Option<Highlight>,
}

impl Search {
    pub fn cancel(&mut self) {
        self.pending = None;
        self.feedback = None;
    }
    pub fn clear(&mut self) {
        self.cancel();
        self.query.clear();
        self.highlight = None;
    }
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Exact => Mode::Fuzzy,
            Mode::Fuzzy => Mode::Exact,
        };
        self.cancel();
        self.highlight = None;
    }
    pub fn begin(
        &mut self,
        browser: &mut SourceBrowser,
        query: Option<String>,
        backward: bool,
    ) -> Result<Request, String> {
        let text = query.as_deref().unwrap_or(&self.query);
        SearchQuery::new(text, self.mode.matching()).map_err(|error| error.to_string())?;
        if browser.pending_generation().is_some() {
            return Err("Wait for source loading before searching".into());
        }
        let document = browser
            .document()
            .ok_or("Open a source file before searching")?;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let request = Request {
            generation: self.next_generation,
            source: document.source.clone(),
            cursor: document.cursor,
            browser_revision: browser.revision(),
            query: text.into(),
            mode: self.mode,
            backward,
            include_origin: query.is_some(),
        };
        self.query.clone_from(&request.query);
        self.pending = Some(request.clone());
        self.feedback = None;
        self.highlight = None;
        browser.set_following(false);
        Ok(request)
    }
    pub fn status(&self, source: &SourceId) -> Option<&str> {
        if self
            .pending
            .as_ref()
            .is_some_and(|request| request.source.id() == *source)
        {
            return Some("Searching whole file… Esc cancels");
        }
        self.feedback
            .as_ref()
            .filter(|(id, _)| id == source)
            .map(|(_, text)| text.as_str())
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn highlight(&self, source: &SourceId, line: usize, text: &str) -> Option<(usize, usize)> {
        self.highlight
            .as_ref()
            .filter(|hit| hit.source == *source && hit.line == line && hit.text == text)
            .map(|hit| (hit.start, hit.end))
    }
    pub fn finish(
        &mut self,
        browser: &mut SourceBrowser,
        generation: u64,
        result: Result<Outcome, String>,
    ) -> Option<String> {
        if self
            .pending
            .as_ref()
            .is_none_or(|request| request.generation != generation)
        {
            return None;
        }
        let request = self.pending.take().expect("generation checked above");
        if browser.revision() != request.browser_revision
            || browser.pending_generation().is_some()
            || browser.document().is_none_or(|document| {
                document.source.id() != request.source.id() || document.cursor != request.cursor
            })
        {
            return None;
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                return Some(self.feedback(request.source.id(), format!("Search failed: {error}")))
            }
        };
        if outcome.content.source.id() != request.source.id() {
            return Some(self.feedback(
                request.source.id(),
                "Search returned a different source identity".into(),
            ));
        }
        let old_column = browser.document().expect("document checked above").column;
        let line = outcome
            .found
            .as_ref()
            .map_or(request.cursor, |hit| hit.line as usize)
            .clamp(1, outcome.content.total_lines.max(1) as usize);
        let content = &outcome.content;
        let valid_window = content.start_line > 0
            && content.line_count <= 240
            && if content.total_lines == 0 {
                content.start_line == 1 && content.line_count == 0 && content.content.is_empty()
            } else {
                content.line_count > 0
                    && content.content.split('\n').count() == content.line_count as usize
                    && content.start_line as u64 + content.line_count as u64 - 1
                        <= content.total_lines as u64
                    && (content.start_line as usize
                        ..content.start_line as usize + content.line_count as usize)
                        .contains(&line)
            };
        if !valid_window {
            return Some(self.feedback(
                request.source.id(),
                "Search returned an invalid source window".into(),
            ));
        }
        let highlight = if let Some(hit) = &outcome.found {
            let offset = hit.line.checked_sub(outcome.content.start_line);
            let text = offset
                .filter(|offset| *offset < outcome.content.line_count)
                .and_then(|offset| outcome.content.content.split('\n').nth(offset as usize));
            let Some(text) = text.filter(|text| {
                hit.start_column > 0
                    && hit.end_column > hit.start_column
                    && hit.end_column as usize <= text.chars().count() + 1
            }) else {
                return Some(self.feedback(
                    request.source.id(),
                    "Search returned an invalid match window".into(),
                ));
            };
            Some(Highlight {
                source: request.source.id(),
                line,
                start: hit.start_column as usize - 1,
                end: hit.end_column as usize - 1,
                text: text.into(),
            })
        } else {
            None
        };
        let column = highlight.as_ref().map_or(old_column, |hit| {
            crate::display_text::width(&hit.text.chars().take(hit.start).collect::<String>())
                .saturating_sub(8)
        });
        let navigation = browser.go_to_line(line).expect("document checked above");
        if !browser.apply(navigation.generation, outcome.content) {
            return None;
        }
        // Same-file navigation preserves the existing horizontal position.
        // The match column is an absolute destination, not a scroll delta.
        browser.scroll_horizontal(column as isize - old_column as isize);
        self.highlight = highlight;
        let message = match outcome.found {
            Some(hit) => format!(
                "{}line {} · n next / N previous",
                if hit.wrapped { "Wrapped · " } else { "" },
                hit.line
            ),
            None => "No matching line in the whole file".into(),
        };
        Some(self.feedback(request.source.id(), message))
    }
    fn feedback(&mut self, source: SourceId, message: String) -> String {
        self.feedback = Some((source, message.clone()));
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_browser::tests::remote_source;
    use ddb_api_client::v2;

    fn content(text: &str, first: u32, total: u32) -> Content {
        Content {
            source: remote_source(v2::SourceFile {
                source_reference: "search/file".into(),
                name: "main.rs".into(),
                ..Default::default()
            }),
            start_line: first,
            line_count: if total == 0 {
                0
            } else {
                text.split('\n').count() as u32
            },
            total_lines: total,
            content: text.into(),
        }
    }

    fn browser() -> SourceBrowser {
        let mut browser = SourceBrowser::default();
        let data = content(
            &format!("{}\nsecond line", "original wide line ".repeat(10)),
            1,
            1000,
        );
        let request = browser.open_source(data.source.clone());
        assert!(browser.apply(request.generation, data));
        browser.fit_horizontal(20);
        browser.scroll_horizontal(30);
        browser
    }

    fn outcome() -> Outcome {
        Outcome {
            found: Some(ddb_source_workspace::SearchMatch {
                line: 999,
                start_column: 4,
                end_column: 8,
                wrapped: false,
            }),
            content: content("x🦀 café_Δelta", 999, 1000),
        }
    }

    #[test]
    fn pinned_result_navigates_beyond_loaded_window_and_resets_horizontal_position() {
        let mut browser = browser();
        let mut search = Search::default();
        let request = search
            .begin(&mut browser, Some("café".into()), false)
            .unwrap();
        assert!(request.include_origin);
        assert!(!browser.following());
        assert!(search.pending());
        assert!(search
            .status(&request.source.id())
            .unwrap()
            .contains("Searching"));
        assert!(search
            .finish(&mut browser, request.generation, Ok(outcome()))
            .unwrap()
            .contains("999"));
        assert!(!search.pending());
        let doc = browser.document().unwrap();
        assert_eq!(doc.cursor, 999);
        assert_eq!(
            doc.column, 0,
            "old horizontal position must not be added twice"
        );
        assert_eq!(
            search.highlight(&doc.source.id(), 999, &doc.lines[0]),
            Some((3, 7))
        );
        assert_eq!(
            search.highlight(&doc.source.id(), 999, "changed source"),
            None
        );
        let previous = search.begin(&mut browser, None, true).unwrap();
        assert!(previous.backward);
        assert!(!previous.include_origin);
        assert_eq!(previous.query, "café");
    }

    #[test]
    fn stale_results_do_not_override_cursor_navigation_replacement_or_restart() {
        for mutation in 0..4 {
            let mut browser = browser();
            let mut search = Search::default();
            let old = search
                .begin(&mut browser, Some("old".into()), false)
                .unwrap();
            match mutation {
                0 => browser.set_cursor(2),
                1 => {
                    browser.go_to_line(3);
                }
                2 => browser.set_server_instance("restarted".into()),
                _ => search.cancel(),
            }
            assert!(search
                .finish(&mut browser, old.generation, Ok(outcome()))
                .is_none());
            assert!(!search.pending());
        }
        let mut browser = browser();
        let mut search = Search::default();
        let old = search
            .begin(&mut browser, Some("old".into()), false)
            .unwrap();
        let latest = search
            .begin(&mut browser, Some("new".into()), false)
            .unwrap();
        assert!(search
            .finish(&mut browser, old.generation, Ok(outcome()))
            .is_none());
        assert!(search.pending());
        search.finish(&mut browser, latest.generation, Err("offline".into()));
        assert!(!search.pending());
        assert!(search
            .status(&latest.source.id())
            .unwrap()
            .contains("offline"));
    }

    #[test]
    fn no_match_replaces_changed_source_and_preserves_horizontal_scroll() {
        let mut browser = browser();
        let mut search = Search::default();
        let request = search
            .begin(&mut browser, Some("missing".into()), false)
            .unwrap();
        let data = content(&"replacement ".repeat(10), 1, 1);
        search.finish(
            &mut browser,
            request.generation,
            Ok(Outcome {
                found: None,
                content: data,
            }),
        );
        let doc = browser.document().unwrap();
        assert!(doc.lines[0].starts_with("replacement"));
        assert_eq!(doc.column, 30);
        assert!(search
            .status(&doc.source.id())
            .unwrap()
            .contains("No matching"));
        browser.set_cursor(1);
        let request = search.begin(&mut browser, None, false).unwrap();
        search.finish(
            &mut browser,
            request.generation,
            Ok(Outcome {
                found: None,
                content: content("", 1, 0),
            }),
        );
        assert_eq!(browser.document().unwrap().total_lines, 0);
    }

    #[test]
    fn malformed_results_are_rejected_without_changing_source() {
        for corruption in 0..5 {
            let mut browser = browser();
            let mut search = Search::default();
            let request = search
                .begin(&mut browser, Some("café".into()), false)
                .unwrap();
            let mut result = outcome();
            match corruption {
                0 => result.found.as_mut().unwrap().start_column = 0,
                1 => result.found.as_mut().unwrap().end_column = 100,
                2 => result.content.line_count = 241,
                3 => result.content.total_lines = 2,
                _ => {
                    result.content.source = remote_source(v2::SourceFile {
                        source_reference: "other".into(),
                        ..Default::default()
                    })
                }
            }
            assert!(search
                .finish(&mut browser, request.generation, Ok(result))
                .is_some());
            assert_eq!(browser.document().unwrap().cursor, 1);
            assert!(!search.pending());
        }
    }

    #[test]
    fn mode_toggle_and_clear_cancel_without_losing_query_whitespace() {
        let mut browser = browser();
        let mut search = Search::default();
        search
            .begin(&mut browser, Some(" café ".into()), false)
            .unwrap();
        search.toggle_mode();
        assert_eq!(search.mode, Mode::Fuzzy);
        assert_eq!(search.query, " café ");
        assert!(!search.pending());
        search.clear();
        assert!(search.query.is_empty());
        assert!(search.begin(&mut browser, None, false).is_err());
        browser.go_to_line(2);
        assert!(search.begin(&mut browser, Some("x".into()), false).is_err());
    }
}
