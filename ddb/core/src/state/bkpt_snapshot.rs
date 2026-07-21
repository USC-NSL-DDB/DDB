use serde::Serialize;

use crate::state::{BkptLoc, BkptMeta, SubBkptMeta, SubBkptType};

/// Published snapshot of a breakpoint aggregate.
///
/// This is the stable contract emitted with breakpoint effects; interface
/// layers serialize it as-is instead of reaching into aggregate internals.
#[derive(Clone, Debug, Serialize)]
pub struct BreakpointSnapshot {
    id: u64,
    location: BreakpointLocationSnapshot,
    enabled: bool,
    times: u64,
    subbkpts: Vec<SubBreakpointSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BreakpointLocationSnapshot {
    src: String,
    line: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub enum SubBreakpointSnapshot {
    #[serde(rename = "session")]
    Session {
        id: u64,
        target_session: u64,
        local_breakpoint_id: u64,
    },
    #[serde(rename = "group")]
    Group {
        id: u64,
        target_group: u64,
        active_sessions: usize,
    },
}

impl BreakpointSnapshot {
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl From<&BkptMeta> for BreakpointSnapshot {
    fn from(breakpoint: &BkptMeta) -> Self {
        Self {
            id: breakpoint.id(),
            location: breakpoint.location().into(),
            enabled: breakpoint.is_enabled(),
            times: breakpoint.times(),
            subbkpts: breakpoint
                .sub_breakpoints()
                .iter()
                .map(SubBreakpointSnapshot::from)
                .collect(),
        }
    }
}

impl From<&BkptLoc> for BreakpointLocationSnapshot {
    fn from(location: &BkptLoc) -> Self {
        Self {
            src: location.path().to_string(),
            line: location.line(),
        }
    }
}

impl From<&SubBkptMeta> for SubBreakpointSnapshot {
    fn from(sub_breakpoint: &SubBkptMeta) -> Self {
        match sub_breakpoint.kind() {
            SubBkptType::Session(session) => Self::Session {
                id: sub_breakpoint.id(),
                target_session: session.target_session(),
                local_breakpoint_id: session.local_id(),
            },
            SubBkptType::Group(group) => Self::Group {
                id: sub_breakpoint.id(),
                target_group: group.target_group().value(),
                active_sessions: group.local_ids().len(),
            },
        }
    }
}
