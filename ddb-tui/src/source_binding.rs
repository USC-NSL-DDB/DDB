//! Debugger association of a document. Display/content paths never imply a
//! debugger path, and equal filenames never imply equal session ownership.

use crate::model::Breakpoint;
use ddb_api_client::v2::{self, target};

#[derive(Clone, Copy)]
pub struct DebuggerBinding<'a> {
    pub path: &'a str,
    pub owners: &'a [v2::SourceOwner],
}

impl<'a> DebuggerBinding<'a> {
    pub fn from_source(source: &'a v2::SourceFile) -> Option<Self> {
        Some(Self {
            path: source
                .debugger_path
                .as_deref()
                .filter(|path| !path.is_empty())?,
            owners: &source.owners,
        })
    }

    pub fn single_owner(self) -> Option<&'a str> {
        let [owner] = self.owners else { return None };
        Some(&owner.session_id)
    }

    pub fn matches_location(self, session_id: &str, path: &str) -> bool {
        self.path == path
            && self
                .owners
                .iter()
                .any(|owner| owner.session_id == session_id)
    }

    pub fn marks_breakpoint(
        self,
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
                    self.owners
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
