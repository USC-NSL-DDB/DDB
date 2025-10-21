use std::collections::HashSet;

use dashmap::DashMap;

use super::{get_group_mgr, GroupId};

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct BkptMeta {
    orig_cmd: String,
    // src: String,
    // line: u64,
    // enabled: bool,
}

impl BkptMeta {
    pub fn new(orig_cmd: String) -> Self {
        BkptMeta { orig_cmd }
    }

    pub fn get_cmd(&self) -> &String {
        &self.orig_cmd
    }
}

#[derive(Debug)]
pub struct BreakpointMgr {
    // maps from a group_id to a set of breakpoints
    // maybe filtering is needed?
    bkpts: DashMap<GroupId, HashSet<BkptMeta>>,
    // bkpts that are pending for adding confirmation
    // pending_bkpts: DashMap<u64, BkptMeta>,
}

impl BreakpointMgr {
    pub fn new() -> Self {
        BreakpointMgr {
            bkpts: DashMap::new(),
            // pending_bkpts: DashMap::new(),
        }
    }

    // used when DDB initiate a breakpoint insertion
    // This breakpoint is not yet considered as a valid one
    // until the dbg end confirm it, e.g. emitting breakpoint event.
    // pub fn pending_add(&self, id: u64, cmd: String) {
    //     let bkpt = BkptMeta::new(cmd);
    //     self.pending_bkpts.insert(id, bkpt);
    // }

    // pub fn confirm_add(&self, id: u64, sid: u64) {
    //     if let Some(grp_id) = get_group_mgr().get_group_id_by_sid(sid) {
    //         if let Some((_, bkpt)) = self.pending_bkpts.remove(&id) {
    //             self.add(&grp_id, bkpt);
    //         }
    //     }
    // }

    pub fn add(&self, grp_id: &GroupId, bkpt: BkptMeta) {
        self.bkpts.entry(grp_id.clone()).or_default().insert(bkpt);
    }

    pub fn add_by_sid(&self, sid: u64, bkpt: BkptMeta) {
        if let Some(grp_id) = get_group_mgr().get_group_id_by_sid(sid) {
            self.add(&grp_id, bkpt);
        }
    }

    pub fn get(&self, grp_id: &GroupId) -> Option<HashSet<BkptMeta>> {
        self.bkpts.get(grp_id).map(|v| v.clone())
    }

    pub fn get_by_sid(&self, sid: u64) -> Option<HashSet<BkptMeta>> {
        let grp_id = get_group_mgr().get_group_id_by_sid(sid);
        grp_id.map(|id| self.get(&id)).flatten()
    }

    // This function holds a mutable reference to the entry.
    // Thus, the operation closure should not contain any await point.
    // Otherwise, it will cause a deadlock.
    // If this is a concern, we can consider swicth the data struct.
    pub fn modify<F>(&self, grp_id: &GroupId, op: F)
    where
        F: FnOnce(&mut HashSet<BkptMeta>),
    {
        if let Some(mut entry) = self.bkpts.get_mut(grp_id) {
            op(&mut entry.value_mut());
        }
    }
}
