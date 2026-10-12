//! Debugger association of a document. Display/content paths never imply a
//! debugger path, and equal filenames never imply equal session ownership.

use crate::model::Breakpoint;
use crate::source_types::Source;
use ddb_api_client::v2::{self, target};
use std::borrow::Cow;

#[derive(Clone)]
pub struct DebuggerBinding<'a> {
    pub path: Cow<'a, str>,
    pub owners: Cow<'a, [v2::SourceOwner]>,
    /// An unscoped local workspace is a view of this exact debugger path
    /// across sessions, not a claim that the document belongs to one session.
    unscoped_local: bool,
}

impl<'a> DebuggerBinding<'a> {
    pub fn from_source(source: &'a Source, sessions: &'a [v2::Session]) -> Option<Self> {
        Some(Self {
            path: source.debugger_path()?,
            owners: source.owners(sessions),
            unscoped_local: matches!(source, Source::Local(local)
                if local.root.session_tags.is_empty() && local.owner_hint.is_empty()),
        })
    }

    pub fn single_owner(&self) -> Option<&str> {
        let [owner] = self.owners.as_ref() else {
            return None;
        };
        Some(&owner.session_id)
    }

    pub fn matches_location(&self, session_id: &str, path: &str) -> bool {
        self.path == path
            && (self.unscoped_local
                || self
                    .owners
                    .iter()
                    .any(|owner| owner.session_id == session_id))
    }

    pub fn marks_breakpoint(
        &self,
        breakpoint: &Breakpoint,
        line: usize,
        sessions: &[v2::Session],
    ) -> bool {
        let resolved = breakpoint.sub_breakpoints.iter().any(|member| {
            member.location.as_ref().is_some_and(|location| {
                location.line as usize == line
                    && location
                        .path
                        .as_deref()
                        .is_some_and(|path| self.matches_location(&member.session_id, path))
            })
        });
        resolved
            || (breakpoint.location.line as usize == line
                && breakpoint.location.src == self.path
                && breakpoint.target.as_ref().is_some_and(|target| {
                    self.unscoped_local
                        || self
                            .owners
                            .iter()
                            .any(|owner| target_includes(target, &owner.session_id, sessions))
                }))
    }
}

fn target_includes(target: &v2::Target, session_id: &str, sessions: &[v2::Session]) -> bool {
    match target.selector.as_ref() {
        Some(target::Selector::Session(value)) => value.session_id == session_id,
        Some(target::Selector::SessionSet(value)) => {
            value.session_ids.iter().any(|id| id == session_id)
        }
        Some(target::Selector::Group(value)) => sessions.iter().any(|session| {
            session.session_id == session_id
                && session.group_id.as_deref() == Some(value.group_id.as_str())
        }),
        Some(target::Selector::Multiple(value)) => value
            .targets
            .iter()
            .any(|target| target_includes(target, session_id, sessions)),
        Some(target::Selector::Broadcast(_)) => sessions
            .iter()
            .any(|session| session.session_id == session_id),
        // Dynamic targets and thread targets require a resolved sub-breakpoint.
        _ => false,
    }
}
