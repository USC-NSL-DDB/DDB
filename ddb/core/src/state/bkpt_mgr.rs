use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc,
    },
};

use dashmap::DashMap;

use super::{get_group_mgr, GroupId};
use crate::{
    common::counter::SimpleCounter,
    state::{get_bkpt_mgr, SessionId},
};

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

impl BkptLoc {
    pub fn get_path(&self) -> &str {
        &self.src
    }

    pub fn get_line(&self) -> u64 {
        self.line
    }

    pub fn to_bkpt_path(&self) -> String {
        format!("{}:{}", self.src, self.line)
    }
}

impl From<[&str; 2]> for BkptLoc {
    fn from(arr: [&str; 2]) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        BkptLoc { src, line }
    }
}

impl From<&[&str; 2]> for BkptLoc {
    fn from(arr: &[&str; 2]) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        BkptLoc { src, line }
    }
}

impl From<Vec<&str>> for BkptLoc {
    fn from(arr: Vec<&str>) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        BkptLoc { src, line }
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

    pub fn get_target_session(&self) -> u64 {
        self.target_session
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

    pub fn get_target_group(&self) -> GroupId {
        self.target_group
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
            }
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
            }
        }
    }

    pub fn get_id(&self) -> u64 {
        self.id
    }

    pub fn get_type(&self) -> &SubBkptType {
        &self.subbkpt_type
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

    fn add_subbkpt(&mut self, subbkpt_type: SubBkptType) {
        let subbkpt_id = self.sub_bkpt_counter.next();
        let subbkpt = SubBkptMeta::new(subbkpt_id, self.id, subbkpt_type);
        match &subbkpt.subbkpt_type {
            // mark this subbkpt is a group breakpoint
            SubBkptType::Group(group_subbkpt) => {
                get_bkpt_mgr().add_grp_bkpt(group_subbkpt.target_group, self.id);
            }
            _ => {}
        }
        self.subbkpts.push(subbkpt);
    }

    fn delete_subbkpt(&mut self, subbkpt_id: u64) {
        self.subbkpts.retain(|sb| {
            if sb.id != subbkpt_id {
                true
            } else {
                match &sb.subbkpt_type {
                    SubBkptType::Group(group_subbkpt) => {
                        get_bkpt_mgr().delete_grp_bkpt(group_subbkpt.target_group, self.id);
                    }
                    _ => {}
                }
                false
            }
        });
    }

    fn remove_all_subbkpts(&mut self) {
        for sb in &self.subbkpts {
            match &sb.subbkpt_type {
                SubBkptType::Group(group_subbkpt) => {
                    get_bkpt_mgr().delete_grp_bkpt(group_subbkpt.target_group, self.id);
                }
                _ => {}
            }
        }
        self.subbkpts.clear();
    }

    pub fn enable(&self) {
        self.enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn disable(&self) {
        self.enabled
            .store(false, std::sync::atomic::Ordering::SeqCst);
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

    pub fn get_id(&self) -> u64 {
        self.id
    }

    pub fn get_subbkpts(&self) -> &Vec<SubBkptMeta> {
        &self.subbkpts
    }

    pub fn update_grp_bkpt<F: Fn(&GroupSubBkpt)>(&self, grp_id: GroupId, f: F) {
        for sb in &self.subbkpts {
            match &sb.subbkpt_type {
                SubBkptType::Group(group_subbkpt) => {
                    if group_subbkpt.target_group == grp_id {
                        // update logic
                        // group_subbkpt.add_local_bkpt(1, 1);
                        f(group_subbkpt);
                    }
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug)]
pub struct BreakpointMgr {
    // maps from a group_id to a set of breakpoints
    // maybe filtering is needed?
    // bkpts: DashMap<GroupId, HashSet<BkptMeta>>,
    bkpts: DashMap<u64, BkptMeta>,

    // reverse index from (session_id, local_bkpt_id) to (global_bkpt_id, sub_bkpt_id)
    local_bkpt_to_global: DashMap<(SessionId, u64), (u64, u64)>,

    // maps from group id to bkpt IDs, manages all breakpoints set in a group
    group_bkpt: DashMap<GroupId, HashSet<u64>>,
    // bkpts that are pending for adding confirmation
    // pending_bkpts: DashMap<u64, BkptMeta>,
}

impl BreakpointMgr {
    pub fn new() -> Self {
        BreakpointMgr {
            bkpts: DashMap::new(),
            local_bkpt_to_global: DashMap::new(),
            group_bkpt: DashMap::new(),
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

    // pub fn update_bkpt(&self, bkpt_id: u64) {
    // }

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

    fn add_grp_bkpt(&self, grp_id: GroupId, bkpt_id: u64) {
        self.group_bkpt.entry(grp_id).or_default().insert(bkpt_id);
    }

    fn delete_grp_bkpt(&self, grp_id: GroupId, bkpt_id: u64) {
        if let Some(mut entry) = self.group_bkpt.get_mut(&grp_id) {
            entry.value_mut().remove(&bkpt_id);
        }
    }

    fn insert_local_bkpt_id_index(
        &self,
        session_id: SessionId,
        local_bkpt_id: u64,
        major_bkpt_id: u64,
        sub_bkpt_id: u64,
    ) {
        self.local_bkpt_to_global
            .insert((session_id, local_bkpt_id), (major_bkpt_id, sub_bkpt_id));
    }

    pub fn get_bkpts_by_grp_id(&self, grp_id: GroupId) -> Vec<BkptMeta> {
        let mut res = Vec::new();
        if let Some(entry) = self.group_bkpt.get(&grp_id) {
            for bkpt_id in entry.iter() {
                if let Some(bkpt_entry) = self.bkpts.get(bkpt_id) {
                    res.push(bkpt_entry.value().clone());
                }
            }
        }
        res
    }

    pub fn get_bkpt_locs_by_grp_id(&self, grp_id: GroupId) -> Vec<BkptLoc> {
        let mut res = Vec::new();
        if let Some(entry) = self.group_bkpt.get(&grp_id) {
            for bkpt_id in entry.iter() {
                if let Some(bkpt_entry) = self.bkpts.get(bkpt_id) {
                    res.push(bkpt_entry.value().loc.clone());
                }
            }
        }
        res
    }

    pub fn update_subbkpts_with<F: Fn(&mut SubBkptMeta)>(&self, bkpt_id: u64, f: F) {
        self.bkpts.entry(bkpt_id).and_modify(|bkpt| {
            for sb in bkpt.subbkpts.iter_mut() {
                f(sb);
            }
        });
    }

    pub fn get_bkpts_by_id(&self, bkpt_id: u64) -> Option<BkptMeta> {
        self.bkpts.get(&bkpt_id).map(|entry| entry.value().clone())
    }

    pub fn get_local_bkpt_ids(&self, bkpt_id: u64) -> Vec<(SessionId, u64)> {
        let mut res = Vec::new();
        for entry in self.local_bkpt_to_global.iter() {
            let ((sess_id, local_bkpt_id), (global_bkpt_id, _)) = entry.pair();
            if *global_bkpt_id == bkpt_id {
                res.push((*sess_id, *local_bkpt_id));
            }
        }
        res
    }

    // pub fn update_grp_bkpt_in_bkpt<F: Fn(&GroupSubBkpt)>(
    //     &self,
    //     bkpt_id: u64,
    //     grp_id: GroupId,
    //     f: F,
    // ) {
    //     if let Some(bkpt_entry) = self.bkpts.get(&bkpt_id) {
    //         bkpt_entry.value().update_grp_bkpt(grp_id, f);
    //     }
    // }

    pub fn setup_grp_bkpt_for_new_session(
        &self,
        bkpt_id: u64,
        grp_id: GroupId,
        sid: SessionId,
        local_bkpt_id: u64,
    ) {
        self.update_subbkpts_with(bkpt_id, |subbkpt| {
            match &mut subbkpt.subbkpt_type {
                SubBkptType::Group(group_subbkpt) => {
                    if group_subbkpt.target_group == grp_id {
                        group_subbkpt.add_local_bkpt(sid, local_bkpt_id);
                        // register the local bkpt id index
                        self.insert_local_bkpt_id_index(
                            sid,
                            local_bkpt_id,
                            bkpt_id,
                            subbkpt.id,
                        );
                    }
                }
                _ => {}
            }
        });
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
