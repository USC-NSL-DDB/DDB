pub mod proclet_ctrl;
pub mod proclet_restore;

use std::sync::OnceLock;
use proclet_restore::ProcletRestorationMgr;

// re-export just for being lazy...
fn next_rpc_req_id() -> u64 {
    crate::common::counter::next_rpc_req_id()
}

static PROCLET_RESTORE: OnceLock<ProcletRestorationMgr> = OnceLock::new();

pub fn get_proclet_restore_mgr() -> &'static ProcletRestorationMgr {
    PROCLET_RESTORE.get_or_init(ProcletRestorationMgr::new)
}
