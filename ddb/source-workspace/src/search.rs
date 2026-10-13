//! Whole-document matching over the existing immutable line index.

use super::{Document, Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchMode {
    /// Case-sensitive, contiguous text on one source line.
    Exact,
    /// Case-insensitive Unicode lowercase subsequence on one source line.
    Fuzzy,
}

#[derive(Clone, Debug)]
pub struct SearchQuery {
    text: String,
    mode: SearchMode,
    folded: Vec<char>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub line: u32,
    /// One-based Unicode scalar column, inclusive.
    pub start_column: u32,
    /// One-based Unicode scalar column, exclusive. For fuzzy matches this is
    /// the bounding range, including gaps between matched characters.
    pub end_column: u32,
    pub wrapped: bool,
}

impl SearchQuery {
    pub fn new(text: &str, mode: SearchMode) -> Result<Self> {
        if text.is_empty() || text.len() > 1024 || text.chars().any(|c| c.is_control() && c != '\t')
        {
            return Err(Error::InvalidSearch(
                "query must contain 1–1024 UTF-8 bytes on one line",
            ));
        }
        Ok(Self {
            text: text.into(),
            mode,
            folded: text.chars().flat_map(char::to_lowercase).collect(),
        })
    }

    fn columns(&self, line: &str) -> Option<(u32, u32)> {
        match self.mode {
            SearchMode::Exact => {
                let byte = line.find(&self.text)?;
                let first = line[..byte].chars().count() as u32 + 1;
                Some((first, first + self.text.chars().count() as u32))
            }
            SearchMode::Fuzzy => {
                let mut next = 0;
                let mut first = None;
                for (column, character) in line.chars().enumerate() {
                    for folded in character.to_lowercase() {
                        if folded == self.folded[next] {
                            first.get_or_insert(column as u32 + 1);
                            next += 1;
                            if next == self.folded.len() {
                                return Some((
                                    first.expect("at least one match"),
                                    column as u32 + 2,
                                ));
                            }
                        }
                    }
                }
                None
            }
        }
    }
}

impl Document {
    /// Find the next matching line, wrapping once across the whole document.
    /// Each line is visited at most once; only one match is returned per line.
    /// No source window is allocated while scanning. The caller must use a
    /// bounded blocking executor, just as for reading a source document.
    pub fn search(
        &self,
        query: &SearchQuery,
        origin_line: u32,
        backward: bool,
        include_origin: bool,
    ) -> Option<SearchMatch> {
        let len = self.starts.len();
        if len == 0 {
            return None;
        }
        let origin = origin_line.saturating_sub(1).min(len as u32 - 1) as usize;
        let match_line = |index: usize, wrapped| {
            let end = self
                .starts
                .get(index + 1)
                .copied()
                .unwrap_or(self.text.len());
            let line = &self.text[self.starts[index]..end];
            let line = match line.strip_suffix('\n') {
                Some(line) => line.strip_suffix('\r').unwrap_or(line),
                None => line,
            };
            let (start_column, end_column) = query.columns(line)?;
            Some(SearchMatch {
                line: index as u32 + 1,
                start_column,
                end_column,
                wrapped,
            })
        };
        if backward {
            let end = origin + usize::from(include_origin);
            (0..end)
                .rev()
                .find_map(|i| match_line(i, false))
                .or_else(|| (end..len).rev().find_map(|i| match_line(i, true)))
        } else {
            let start = origin + usize::from(!include_origin);
            (start..len)
                .find_map(|i| match_line(i, false))
                .or_else(|| (0..start).find_map(|i| match_line(i, true)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, SourceRoot};
    use std::sync::Arc;

    fn document(text: &str) -> Arc<Document> {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("main"), text).unwrap();
        SourceRoot::open(directory.path(), Limits::default())
            .unwrap()
            .read("main")
            .unwrap()
    }

    #[test]
    fn exact_search_visits_the_entire_file_and_wraps_in_both_directions() {
        let doc = document(&format!("first match\n{}last match\n", "no\n".repeat(1000)));
        let query = SearchQuery::new("match", SearchMode::Exact).unwrap();
        let next = doc.search(&query, 1, false, false).unwrap();
        assert_eq!(next.line, 1002);
        assert!(!next.wrapped);
        assert_eq!(doc.search(&query, 1002, false, false).unwrap().line, 1);
        assert!(doc.search(&query, 1002, false, false).unwrap().wrapped);
        assert_eq!(doc.search(&query, 1002, true, false).unwrap().line, 1);
        assert_eq!(doc.search(&query, 1, true, false).unwrap().line, 1002);
        assert_eq!(doc.search(&query, 1, false, true).unwrap().line, 1);
        assert!(doc
            .search(
                &SearchQuery::new("MATCH", SearchMode::Exact).unwrap(),
                1,
                false,
                true
            )
            .is_none());
    }

    #[test]
    fn unicode_fuzzy_matching_is_case_insensitive_and_reports_scalar_columns() {
        let doc = document("x🦀 café_Δelta\r\nİstanbul\n");
        let query = SearchQuery::new("🦀CÉΔL", SearchMode::Fuzzy).unwrap();
        let found = doc.search(&query, 1, false, true).unwrap();
        assert_eq!(
            (found.line, found.start_column, found.end_column),
            (1, 2, 12)
        );
        assert!(doc
            .search(
                &SearchQuery::new("i\u{307}s", SearchMode::Fuzzy).unwrap(),
                1,
                false,
                true
            )
            .is_some());
        let exact = doc
            .search(
                &SearchQuery::new("café", SearchMode::Exact).unwrap(),
                1,
                false,
                true,
            )
            .unwrap();
        assert_eq!((exact.start_column, exact.end_column), (4, 8));
    }

    #[test]
    fn empty_files_invalid_queries_and_single_matching_lines_are_explicit() {
        assert!(document("")
            .search(
                &SearchQuery::new("x", SearchMode::Exact).unwrap(),
                1,
                false,
                true
            )
            .is_none());
        for invalid in ["", "line\nbreak", "nul\0", &"x".repeat(1025)] {
            assert!(SearchQuery::new(invalid, SearchMode::Fuzzy).is_err());
        }
        let doc = document("\tneedle\r\n");
        let query = SearchQuery::new("needle", SearchMode::Exact).unwrap();
        assert!(doc.search(&query, 1, false, false).unwrap().wrapped);
        assert!(doc
            .search(
                &SearchQuery::new("none", SearchMode::Exact).unwrap(),
                0,
                true,
                true
            )
            .is_none());
        assert!(doc
            .search(
                &SearchQuery::new("\t", SearchMode::Exact).unwrap(),
                1,
                false,
                true
            )
            .is_some());
    }
}
