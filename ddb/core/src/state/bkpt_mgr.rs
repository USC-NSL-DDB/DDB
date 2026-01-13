use std::sync::{Arc, atomic::{AtomicBool, AtomicU64}};

use dashmap::DashMap;

use crate::{common::counter::SimpleCounter, state::{SessionId, get_bkpt_mgr}};
use super::{get_group_mgr, GroupId};

#[derive(Debug, Clone)]
pub struct GroupBkptTarget {
    group_id: GroupId,
}

#[derive(Debug, Clone)]
pub struct SessionBkptTarget {
    session_id: u64,    
}

#[derive(Debug, Clone)]
pub struct BkptLoc {
    src: String,
    line: u64,
}

impl From<[&str; 2]> for BkptLoc {
    fn from(arr: [&str; 2]) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        BkptLoc {
            src,
            line,
        }
    }
}   

impl From<&[&str; 2]> for BkptLoc {
    fn from(arr: &[&str; 2]) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        BkptLoc {
            src,
            line,
        }
    }
}   

impl From<Vec<&str>> for BkptLoc {
    fn from(arr: Vec<&str>) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        BkptLoc {
            src,
            line,
        }
    }
}   

#[derive(Debug, Clone)]
pub struct SessionSubBkpt {
    local_id: u64,
    target_session: u64,
}

impl SessionSubBkpt {
    pub fn new(local_id: u64, target_session: u64) -> Self {
        SessionSubBkpt {
            local_id,
            target_session,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GroupSubBkpt {
    // map from session_id to local_bkpt_id
    local_ids: DashMap<u64, u64>,
    target_group: GroupId,
}

impl GroupSubBkpt {
    pub fn new(target_group: GroupId) -> Self {
        GroupSubBkpt {
            local_ids: DashMap::new(),
            target_group,
        }
    }
    
    pub fn add_local_bkpt(&self, session_id: u64, local_bkpt_id: u64) {
        self.local_ids.insert(session_id, local_bkpt_id);
    }
    
    pub fn remove_local_bkpt(&self, session_id: u64) {
        self.local_ids.remove(&session_id);
    }
}

#[derive(Debug, Clone)]
pub enum SubBkptType {
    Session(SessionSubBkpt),
    Group(GroupSubBkpt),
}

#[derive(Debug, Clone)]
pub struct SubBkptMeta {
    id: u64,
    major_bkpt_id: u64,
    subbkpt_type: SubBkptType,
}

impl SubBkptMeta {
    fn new(id: u64, major_bkpt_id: u64, subbkpt_type: SubBkptType) -> Self {
        let bkpt = SubBkptMeta {
            id,
            major_bkpt_id,
            subbkpt_type,
        };
        bkpt.register_local_bkpt_index();
        bkpt
    }
    
    fn register_local_bkpt_index(&self) {
        match &self.subbkpt_type {
            SubBkptType::Session(sess_subbkpt) => {
                let session_id = sess_subbkpt.target_session;
                let local_bkpt_id = sess_subbkpt.local_id;
                get_bkpt_mgr().insert_local_bkpt_id_index(
                    session_id,
                    local_bkpt_id,
                    self.major_bkpt_id,
                    self.id,
                );
            },
            SubBkptType::Group(group_subbkpt) => {
                for entry in group_subbkpt.local_ids.iter() {
                    let session_id = *entry.key();
                    let local_bkpt_id = *entry.value();
                    get_bkpt_mgr().insert_local_bkpt_id_index(
                        session_id,
                        local_bkpt_id,
                        self.major_bkpt_id,
                        self.id,
                    );
                }
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct BkptMeta {
    id: u64,
    subbkpts: Vec<SubBkptMeta>,
    loc: BkptLoc,
    enabled: Arc<AtomicBool>,
    times: Arc<AtomicU64>,
    sub_bkpt_counter: Arc<SimpleCounter>,
}

impl BkptMeta {
    fn new(loc: BkptLoc) -> Self {
        let bkpt_id = crate::common::counter::next_bkpt_id();
        BkptMeta {
            id: bkpt_id,
            subbkpts: Vec::new(),
            loc: loc,
            enabled: Arc::new(AtomicBool::new(true)),
            times: Arc::new(AtomicU64::new(0)),
            sub_bkpt_counter: Arc::new(SimpleCounter::new()),
        }
    }
    
    pub fn add_subbkpt(&mut self, subbkpt_type: SubBkptType) {
        let subbkpt_id = self.sub_bkpt_counter.next();
        let subbkpt = SubBkptMeta::new(subbkpt_id, self.id, subbkpt_type);
        // get_bkpt_mgr().insert_local_bkpt_id_index(session_id, local_bkpt_id, major_bkpt_id, sub_bkpt_id);
        self.subbkpts.push(subbkpt);
    }
    
    pub fn delete_subbkpt(&mut self, subbkpt_id: u64) {
        self.subbkpts.retain(|sb| sb.id != subbkpt_id);
    }
    
    pub fn enable(&self) {
        self.enabled.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    
    pub fn disable(&self) {
        self.enabled.store(false, std::sync::atomic::Ordering::SeqCst);
    }
    
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::SeqCst)
    }
    
    pub fn set_times(&self, times: u64) {
        self.times.store(times, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn get_times(&self) -> u64 {
        self.times.load(std::sync::atomic::Ordering::SeqCst)
    }
    
    pub fn get_loc(&self) -> &BkptLoc {
        &self.loc
    }

    // pub fn get_cmd(&self) -> &String {
    //     // &self.orig_cmd
    // }
}

#[derive(Debug)]
pub struct BreakpointMgr {
    // maps from a group_id to a set of breakpoints
    // maybe filtering is needed?
    // bkpts: DashMap<GroupId, HashSet<BkptMeta>>,
    bkpts: DashMap<u64, BkptMeta>,
    
    // reverse index from (session_id, local_bkpt_id) to (global_bkpt_id, sub_bkpt_id)
    local_bkpt_to_global: DashMap<(SessionId, u64), (u64, u64)>,

    // bkpts that are pending for adding confirmation
    // pending_bkpts: DashMap<u64, BkptMeta>,
}

impl BreakpointMgr {
    pub fn new() -> Self {
        BreakpointMgr {
            bkpts: DashMap::new(),
            local_bkpt_to_global: DashMap::new(),
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
    
    pub fn add_bkpt(&self, loc: BkptLoc) -> u64 {
        let bkpt = BkptMeta::new(loc);
        let bkpt_id = bkpt.id;
        match self.bkpts.insert(bkpt_id, bkpt) {
            Some(_) => panic!("Breakpoint ID collision on {}!", bkpt_id),
            None => bkpt_id,
        }
    }
    
    pub fn delete_bkpt(&self, bkpt_id: u64) {
        self.bkpts.remove(&bkpt_id);
    }
    
    pub fn add_subbkpt(&self, bkpt_id: u64, subbkpt_type: SubBkptType) {
        if let Some(mut bkpt_entry) = self.bkpts.get_mut(&bkpt_id) {
            bkpt_entry.value_mut().add_subbkpt(subbkpt_type);
        }
    }
    
    pub fn delete_subbkpt(&self, bkpt_id: u64, subbkpt_id: u64) {
        if let Some(mut bkpt_entry) = self.bkpts.get_mut(&bkpt_id) {
            bkpt_entry.value_mut().delete_subbkpt(subbkpt_id);
        }
    }
    
    pub fn insert_local_bkpt_id_index(
        &self,
        session_id: SessionId,
        local_bkpt_id: u64,
        major_bkpt_id: u64,
        sub_bkpt_id: u64,
    ) {
        self.local_bkpt_to_global.insert(
            (session_id, local_bkpt_id),
            (major_bkpt_id, sub_bkpt_id),
        );
    }

    // pub fn add(&self, grp_id: GroupId, bkpt: BkptMeta) {
    //     self.bkpts.entry(grp_id).or_default().insert(bkpt);
    // }

    // pub fn add_by_sid(&self, sid: u64, bkpt: BkptMeta) {
    //     if let Some(grp_id) = get_group_mgr().get_grp_id_by_sid(sid) {
    //         self.add(grp_id, bkpt);
    //     }
    // }

    // pub fn get(&self, grp_id: GroupId) -> Option<HashSet<BkptMeta>> {
    //     self.bkpts.get(&grp_id).map(|v| v.clone())
    // }

    // pub fn get_by_sid(&self, sid: u64) -> Option<HashSet<BkptMeta>> {
    //     let grp_id = get_group_mgr().get_grp_id_by_sid(sid);
    //     grp_id.map(|id| self.get(id)).flatten()
    // }

    // This function holds a mutable reference to the entry.
    // Thus, the operation closure should not contain any await point.
    // Otherwise, it will cause a deadlock.
    // If this is a concern, we can consider swicth the data struct.
    // pub fn modify<F>(&self, grp_id: GroupId, op: F)
    // where
    //     F: FnOnce(&mut HashSet<BkptMeta>),
    // {
    //     if let Some(mut entry) = self.bkpts.get_mut(&grp_id) {
    //         op(&mut entry.value_mut());
    //     }
    // }
}
