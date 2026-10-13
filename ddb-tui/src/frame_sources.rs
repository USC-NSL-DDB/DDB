//! Source-read evidence is scoped to a backend instance, session, and exact
//! debugger path. A reported location alone does not establish readability.
use super::*;
use crate::source_types::{Address, Root, Source};

const EVIDENCE_LIMIT: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Key {
    session: String,
    path: String,
}

#[derive(Default)]
pub struct Sources {
    evidence: VecDeque<(Key, Option<String>)>,
    pending: Option<(u64, Vec<Key>)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    NoLocation,
    Unknown,
    Loading,
    Readable,
    Failed,
}

pub struct Status<'a> {
    pub state: State,
    pub error: Option<&'a str>,
}

impl Status<'_> {
    pub fn label(&self) -> &'static str {
        match self.state {
            State::NoLocation => " [no source location]",
            State::Loading => " [loading source]",
            State::Failed => " [source load failed]",
            State::Unknown => " [source unchecked]",
            State::Readable => "",
        }
    }

    pub fn dimmed(&self) -> bool {
        matches!(self.state, State::NoLocation | State::Failed)
    }
}

impl Sources {
    pub fn clear(&mut self) {
        self.evidence.clear();
        self.pending = None;
    }

    pub fn retain_sessions(&mut self, sessions: &[v2::Session]) {
        let present = |key: &Key| {
            sessions
                .iter()
                .any(|session| session.session_id == key.session)
        };
        self.evidence.retain(|(key, _)| present(key));
        if let Some((_, keys)) = &mut self.pending {
            keys.retain(present);
        }
    }
}

impl App {
    fn source_keys(&self, source: &Source) -> Vec<Key> {
        match source {
            Source::Ddb { instance, .. }
                if self.source_server_instance.as_ref() != Some(instance) =>
            {
                return Vec::new()
            }
            Source::Local(source)
                if source.root.session_tags.is_empty()
                    && source.owner_instance != self.source_server_instance =>
            {
                return Vec::new()
            }
            _ => {}
        }
        let Some(path) = source.debugger_path() else {
            return Vec::new();
        };
        source
            .owners(&self.sessions)
            .iter()
            .filter(|owner| {
                self.sessions
                    .iter()
                    .any(|session| session.session_id == owner.session_id)
            })
            .map(|owner| Key {
                session: owner.session_id.clone(),
                path: path.to_string(),
            })
            .collect()
    }

    /// Register only concrete owners. An ambiguous group/broadcast resolution
    /// must not mark every session as unreadable when one lookup fails.
    pub fn begin_frame_source_read(&mut self, request: &SourceRequest) {
        if self.source_browser.pending_generation() != Some(request.generation) {
            return;
        }
        let keys = match &request.address {
            Address::Resolve {
                instance,
                target,
                location,
            } => {
                if instance
                    .as_ref()
                    .is_some_and(|instance| self.source_server_instance.as_ref() != Some(instance))
                {
                    self.frame_sources.pending = None;
                    return;
                }
                let session = match target.as_ref().and_then(|target| target.selector.as_ref()) {
                    Some(target::Selector::Session(target)) => Some(target.session_id.clone()),
                    Some(target::Selector::Thread(target)) => self
                        .threads
                        .iter()
                        .find(|thread| thread.id == target.thread_id)
                        .map(|thread| thread.session_id.clone())
                        .or_else(|| {
                            self.frames
                                .iter()
                                .find(|frame| frame.thread_id == target.thread_id)
                                .map(|frame| frame.session_id.clone())
                        }),
                    _ => None,
                };
                session
                    .zip(location.path.as_ref())
                    .filter(|(session, path)| !session.is_empty() && !path.is_empty())
                    .map(|(session, path)| {
                        vec![Key {
                            session,
                            path: path.clone(),
                        }]
                    })
                    .unwrap_or_default()
            }
            Address::File(source) => self.source_keys(source),
            Address::Export {
                root: Root::Local(root),
                path,
            } => self.source_keys(&Source::Local(crate::source_types::LocalSource {
                root: root.clone(),
                path: path.clone(),
                owner_hint: Vec::new(),
                owner_instance: None,
            })),
            Address::Export { .. } => Vec::new(),
        };
        self.frame_sources.pending = Some((request.generation, keys));
    }

    /// Call only after SourceBrowser accepts the same generation and instance.
    pub fn finish_frame_source_read(&mut self, generation: u64, error: Option<String>) {
        if self
            .frame_sources
            .pending
            .as_ref()
            .is_none_or(|(pending, _)| *pending != generation)
        {
            return;
        }
        let (_, keys) = self
            .frame_sources
            .pending
            .take()
            .expect("generation checked");
        let resolves_displayed_error = error.is_none()
            && self.status_error.as_deref() == Some(self.status.as_str())
            && self.frame_sources.evidence.iter().any(|(key, previous)| {
                keys.contains(key) && previous.as_deref() == Some(self.status.as_str())
            });
        for key in keys {
            self.frame_sources
                .evidence
                .retain(|(previous, _)| previous != &key);
            if self.frame_sources.evidence.len() == EVIDENCE_LIMIT {
                self.frame_sources.evidence.pop_front();
            }
            self.frame_sources.evidence.push_back((key, error.clone()));
        }
        if resolves_displayed_error {
            self.status_error = None;
            self.status = "Source loaded".into();
        }
    }

    pub fn frame_source_status(&self, frame: &StackFrame) -> Status<'_> {
        if frame.file.as_ref().is_none_or(|path| path.is_empty())
            || frame
                .line
                .is_none_or(|line| line == 0 || line > u64::from(u32::MAX))
        {
            return Status {
                state: State::NoLocation,
                error: None,
            };
        }
        let matches = |key: &Key| {
            key.session == frame.session_id && Some(key.path.as_str()) == frame.file.as_deref()
        };
        if self
            .frame_sources
            .pending
            .as_ref()
            .is_some_and(|(generation, keys)| {
                self.source_browser.pending_generation() == Some(*generation)
                    && keys.iter().any(matches)
            })
        {
            return Status {
                state: State::Loading,
                error: None,
            };
        }
        match self
            .frame_sources
            .evidence
            .iter()
            .rev()
            .find(|(previous, _)| matches(previous))
        {
            Some((_, Some(error))) => Status {
                state: State::Failed,
                error: Some(error),
            },
            Some((_, None)) => Status {
                state: State::Readable,
                error: None,
            },
            None => Status {
                state: State::Unknown,
                error: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_evidence_is_bounded_and_oldest_entries_become_unknown() {
        let mut app = App::new("http://localhost".into());
        app.sessions.push(v2::Session {
            session_id: "session".into(),
            ..Default::default()
        });
        let frame = StackFrame {
            id: "frame".into(),
            session_id: "session".into(),
            thread_id: "thread".into(),
            distributed_index: 0,
            level: 0,
            address: String::new(),
            function: "work".into(),
            file: Some("/build/file0.rs".into()),
            line: Some(1),
            boundary: false,
        };
        for index in 0..=EVIDENCE_LIMIT {
            let request = app
                .source_browser
                .navigate(
                    Some(session_target("session")),
                    v2::SourceLocation {
                        path: Some(format!("/build/file{index}.rs")),
                        line: 1,
                        ..Default::default()
                    },
                    crate::source_browser::Navigation::Frame,
                )
                .unwrap();
            app.begin_frame_source_read(&request);
            assert!(app
                .source_browser
                .fail_with_error(request.generation, "missing".into()));
            app.finish_frame_source_read(request.generation, Some("missing".into()));
        }
        assert_eq!(app.frame_sources.evidence.len(), EVIDENCE_LIMIT);
        assert_eq!(app.frame_source_status(&frame).state, State::Unknown);
        let frame = StackFrame {
            file: Some(format!("/build/file{EVIDENCE_LIMIT}.rs")),
            ..frame
        };
        assert_eq!(app.frame_source_status(&frame).state, State::Failed);
    }
}
