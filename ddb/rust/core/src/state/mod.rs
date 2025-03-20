pub mod session_mgr;
pub mod state_mgr;
pub mod thread_mgr;
pub mod source_mgr;
pub mod group_mgr;
pub mod bkpt_mgr;

use std::sync::OnceLock;

pub use bkpt_mgr::*;
pub use session_mgr::*;
pub use state_mgr::*;
pub use thread_mgr::*;
pub use group_mgr::*;
pub use source_mgr::*;

use lazy_static::lazy_static;

lazy_static! {
    pub static ref STATES: StateMgr = StateMgr::new();
}

static GROUPS: OnceLock<GroupMgr> = OnceLock::new();
static SOURCES: OnceLock<SourceMgr> = OnceLock::new();
static BKPTS: OnceLock<BreakpointMgr> = OnceLock::new();

pub fn get_state_mgr() -> &'static StateMgr {
    &STATES
}

pub fn get_group_mgr() -> &'static GroupMgr {
    GROUPS.get_or_init(GroupMgr::new)
}

pub fn get_source_mgr() -> &'static SourceMgr {
    SOURCES.get_or_init(SourceMgr::new)
}

pub fn get_bkpt_mgr() -> &'static BreakpointMgr {
    BKPTS.get_or_init(BreakpointMgr::new)
}
