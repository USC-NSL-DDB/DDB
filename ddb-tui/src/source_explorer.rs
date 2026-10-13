//! Source discovery navigation. Requests and selection never alter debugger state.

use std::collections::VecDeque;

use crate::source_types::{Root, Source, SourceId};
use std::borrow::Cow;

pub const PAGE_SIZE: u32 = 100;
const PAGE_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Roots,
    Catalog,
    Parent,
    Refresh,
    Hidden,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    Roots,
    Directory { root: Root, path: String },
    Catalog,
}

#[derive(Clone, Debug)]
pub enum Entry {
    Root(Root),
    Directory(Node),
    Source(Source),
    Link(Node),
    Other(Node),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub relative_path: String,
}

#[derive(Clone, Debug, PartialEq)]
enum EntryId {
    Root(SourceId),
    Source(SourceId),
    Path(String),
}

impl Entry {
    fn instance(&self) -> Option<&str> {
        match self {
            Self::Root(Root::Ddb { instance, .. }) | Self::Source(Source::Ddb { instance, .. }) => {
                Some(instance)
            }
            _ => None,
        }
    }

    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Root(root) => Cow::Borrowed(root.name()),
            Self::Source(source) => source.path(),
            Self::Directory(entry) | Self::Link(entry) | Self::Other(entry) => {
                Cow::Borrowed(&entry.name)
            }
        }
    }

    fn identity(&self) -> EntryId {
        match self {
            Self::Root(root) => EntryId::Root(root.id()),
            Self::Source(source) => EntryId::Source(source.id()),
            Self::Directory(entry) | Self::Link(entry) | Self::Other(entry) => {
                EntryId::Path(entry.relative_path.clone())
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PageMove {
    Reset,
    Refresh,
    Next,
    Previous,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub generation: u64,
    pub instance: Option<String>,
    pub place: Place,
    pub query: String,
    pub include_hidden: bool,
    pub page_token: Option<String>,
    pub refresh_catalog: bool,
    movement: PageMove,
    selected_identity: Option<EntryId>,
}

#[derive(Debug)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next_token: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum Action {
    Request(Request),
    Open(Source),
    Resolve { root: Root, path: String },
}

#[derive(Debug)]
pub struct Explorer {
    pub visible: bool,
    pub place: Place,
    pub query: String,
    pub include_hidden: bool,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    generation: u64,
    server_instance: Option<String>,
    pending: Option<Request>,
    next_token: Option<String>,
    current_token: Option<String>,
    previous_tokens: VecDeque<Option<String>>,
}

impl Default for Explorer {
    fn default() -> Self {
        Self {
            visible: false,
            place: Place::Roots,
            query: String::new(),
            include_hidden: false,
            entries: Vec::new(),
            selected: 0,
            error: None,
            warnings: Vec::new(),
            generation: 0,
            server_instance: None,
            pending: None,
            next_token: None,
            current_token: None,
            previous_tokens: VecDeque::new(),
        }
    }
}

impl Explorer {
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

    pub fn invalidate_server(&mut self) {
        if matches!(
            self.place,
            Place::Directory {
                root: Root::Local(_),
                ..
            }
        ) {
            return;
        }
        let generation = self.generation;
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    pub fn open(&mut self, catalog: bool) -> Request {
        self.visible = true;
        self.go(if catalog {
            Place::Catalog
        } else {
            Place::Roots
        })
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.pending = None;
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn switch_view(&mut self) -> Request {
        self.go(if self.place == Place::Catalog {
            Place::Roots
        } else {
            Place::Catalog
        })
    }

    fn go(&mut self, place: Place) -> Request {
        self.place = place;
        self.query.clear();
        self.entries.clear();
        self.warnings.clear();
        self.selected = 0;
        self.next_token = None;
        self.current_token = None;
        self.previous_tokens.clear();
        self.request(PageMove::Reset, None, false)
    }

    pub fn parent(&mut self) -> Option<Request> {
        let Place::Directory { root, path } = &self.place else {
            return None;
        };
        if path.is_empty() {
            return Some(self.go(Place::Roots));
        }
        let path = path
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent)
            .to_owned();
        Some(self.go(Place::Directory {
            root: root.clone(),
            path,
        }))
    }

    pub fn refresh(&mut self) -> Request {
        // Always refresh from page one; catalog refresh with a token is invalid.
        self.request(PageMove::Refresh, None, self.place == Place::Catalog)
    }

    pub fn toggle_hidden(&mut self) -> Option<Request> {
        if !matches!(self.place, Place::Directory { .. }) {
            return None;
        }
        self.include_hidden = !self.include_hidden;
        Some(self.refresh())
    }

    pub fn edit_query(&mut self, text: Option<&str>) -> Option<Request> {
        match text {
            Some(text) => {
                for ch in text.chars().filter(|ch| !ch.is_control()) {
                    if self.query.len() + ch.len_utf8() > 1024 {
                        break;
                    }
                    self.query.push(ch);
                }
            }
            None => {
                self.query.pop();
            }
        }
        self.selected = 0;
        if self.place == Place::Catalog {
            self.entries.clear();
            self.warnings.clear();
            self.next_token = None;
            self.current_token = None;
            self.previous_tokens.clear();
            Some(self.request(PageMove::Reset, None, false))
        } else {
            None
        }
    }

    pub fn clear_query(&mut self) -> Option<Request> {
        self.query.clear();
        self.edit_query(Some(""))
    }

    /// Indices into the current bounded page. Catalog filtering is server-side;
    /// root/directory filtering intentionally applies only to the loaded page.
    pub fn visible_indices(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (self.place == Place::Catalog
                    || query.is_empty()
                    || entry.label().to_lowercase().contains(&query))
                .then_some(index)
            })
            .collect()
    }

    pub fn has_previous(&self) -> bool {
        !self.previous_tokens.is_empty()
    }
    pub fn has_next(&self) -> bool {
        self.next_token.is_some()
    }
    pub fn row_count(&self) -> usize {
        self.visible_indices().len()
            + usize::from(self.has_previous())
            + usize::from(self.has_next())
    }

    pub fn select(&mut self, row: usize) {
        self.selected = row.min(self.row_count().saturating_sub(1));
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.select(self.selected.saturating_add_signed(delta));
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        let row = self
            .selected
            .checked_sub(usize::from(self.has_previous()))?;
        self.entries.get(*self.visible_indices().get(row)?)
    }

    /// Right tries a symlink as a directory; Enter opens it as a file. Both
    /// operations remain confined to the provider's explicit root grant.
    pub fn enter_directory(&mut self) -> Option<Action> {
        if !self.loading() {
            if let Some(Entry::Link(entry)) = self.selected_entry() {
                if let Place::Directory { root, .. } = &self.place {
                    let place = Place::Directory {
                        root: root.clone(),
                        path: entry.relative_path.clone(),
                    };
                    return Some(Action::Request(self.go(place)));
                }
            }
        }
        self.activate()
    }

    pub fn activate(&mut self) -> Option<Action> {
        if self.loading() {
            return None;
        }
        let indices = self.visible_indices();
        if self.has_previous() && self.selected == 0 {
            return Some(Action::Request(self.request(
                PageMove::Previous,
                self.previous_tokens.back()?.clone(),
                false,
            )));
        }
        let row = self
            .selected
            .saturating_sub(usize::from(self.has_previous()));
        if row == indices.len() && self.has_next() {
            return Some(Action::Request(self.request(
                PageMove::Next,
                self.next_token.clone(),
                false,
            )));
        }
        match self.entries.get(*indices.get(row)?)?.clone() {
            Entry::Root(root) => Some(Action::Request(self.go(Place::Directory {
                root,
                path: String::new(),
            }))),
            Entry::Directory(entry) => {
                let Place::Directory { root, .. } = &self.place else {
                    return None;
                };
                Some(Action::Request(self.go(Place::Directory {
                    root: root.clone(),
                    path: entry.relative_path,
                })))
            }
            Entry::Source(source) => Some(Action::Open(source)),
            Entry::Link(entry) => {
                let Place::Directory { root, .. } = &self.place else {
                    return None;
                };
                Some(Action::Resolve {
                    root: root.clone(),
                    path: entry.relative_path,
                })
            }
            Entry::Other(_) => {
                self.error = Some("This entry is not a source file or directory".into());
                None
            }
        }
    }

    fn request(
        &mut self,
        movement: PageMove,
        page_token: Option<String>,
        refresh_catalog: bool,
    ) -> Request {
        self.generation = self.generation.wrapping_add(1).max(1);
        let selected_identity = self
            .selected_entry()
            .map(|entry| entry.identity().to_owned());
        let request = Request {
            generation: self.generation,
            instance: self.server_instance.clone(),
            place: self.place.clone(),
            query: self.query.clone(),
            include_hidden: self.include_hidden,
            page_token,
            refresh_catalog,
            movement,
            selected_identity,
        };
        self.error = None;
        self.pending = Some(request.clone());
        request
    }

    pub fn apply(&mut self, generation: u64, page: Page) -> bool {
        if !self.visible
            || self.pending.as_ref().map(|request| request.generation) != Some(generation)
        {
            return false;
        }
        let instance = self.server_instance.clone().or_else(|| {
            page.entries
                .iter()
                .find_map(Entry::instance)
                .map(str::to_owned)
        });
        if page
            .entries
            .iter()
            .filter_map(Entry::instance)
            .any(|entry| Some(entry) != instance.as_deref())
        {
            return self.fail(
                generation,
                "DDB instance changed during source discovery; refresh to retry".into(),
            );
        }
        let token = self
            .pending
            .as_ref()
            .and_then(|request| request.page_token.as_ref());
        if page.entries.len() > PAGE_SIZE as usize
            || page
                .next_token
                .as_ref()
                .is_some_and(|next| next.is_empty() || Some(next) == token)
        {
            return self.fail(
                generation,
                "Source provider returned an invalid page; refresh to retry".into(),
            );
        }
        let request = self.pending.take().expect("generation checked");
        self.server_instance = instance;
        match request.movement {
            PageMove::Next => {
                if self.previous_tokens.len() == PAGE_HISTORY_LIMIT {
                    self.previous_tokens.pop_front();
                }
                self.previous_tokens.push_back(self.current_token.clone());
            }
            PageMove::Previous => {
                self.previous_tokens.pop_back();
            }
            PageMove::Reset | PageMove::Refresh => self.previous_tokens.clear(),
        }
        self.current_token = request.page_token;
        self.next_token = page.next_token;
        self.entries = page.entries;
        self.warnings = page.warnings;
        self.selected = usize::from(self.has_previous() && !self.visible_indices().is_empty());
        if matches!(request.movement, PageMove::Refresh) {
            if let Some(identity) = request.selected_identity {
                if let Some(row) = self
                    .visible_indices()
                    .iter()
                    .position(|index| self.entries[*index].identity() == identity)
                {
                    self.selected = row;
                }
            }
        }
        true
    }

    pub fn fail(&mut self, generation: u64, error: String) -> bool {
        if self.pending.as_ref().map(|request| request.generation) != Some(generation) {
            return false;
        }
        self.pending = None;
        self.error = Some(error);
        true
    }
}

#[cfg(test)]
#[path = "source_explorer_tests.rs"]
mod tests;
