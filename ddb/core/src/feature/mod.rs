pub mod proclet_ctrl;
pub mod proclet_restore;

// re-export just for being lazy...
fn next_rpc_req_id() -> u64 {
    crate::common::counter::next_rpc_req_id()
}
