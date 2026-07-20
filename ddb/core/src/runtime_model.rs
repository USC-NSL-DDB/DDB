use std::sync::Arc;

use crate::state::{BreakpointMgr, GroupMgr, ProcletMgr, StateMgr};

/// Owns the mutable debugger model shared by application services.
///
/// Each repository keeps its specialized synchronization strategy. The model is
/// only a dependency boundary; it does not serialize independent sessions.
pub struct RuntimeModel {
    state: Arc<StateMgr>,
    groups: Arc<GroupMgr>,
    breakpoints: Arc<BreakpointMgr>,
    proclets: Arc<ProcletMgr>,
}

impl RuntimeModel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(StateMgr::new()),
            groups: Arc::new(GroupMgr::new()),
            breakpoints: Arc::new(BreakpointMgr::new()),
            proclets: Arc::new(ProcletMgr::new()),
        })
    }

    pub fn state(&self) -> &Arc<StateMgr> {
        &self.state
    }

    pub fn groups(&self) -> &Arc<GroupMgr> {
        &self.groups
    }

    pub fn breakpoints(&self) -> &Arc<BreakpointMgr> {
        &self.breakpoints
    }

    pub fn proclets(&self) -> &Arc<ProcletMgr> {
        &self.proclets
    }
}
