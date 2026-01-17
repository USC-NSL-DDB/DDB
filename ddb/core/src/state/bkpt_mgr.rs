use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc,
    },
};

use dashmap::DashMap;
use tracing::{debug, warn};

use super::{get_group_mgr, GroupId};
use crate::{
    common::counter::SimpleCounter,
    dbg_parser::gdb_parser::{bkpt_deleted_payload, MIFormatter},
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

    pub fn get_local_bkpt_id(&self) -> u64 {
        self.local_id
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

    pub fn remove_local_bkpt(&self, session_id: u64) -> Option<u64> {
        self.local_ids.remove(&session_id).map(|(_, v)| v)
    }

    pub fn get_target_group(&self) -> GroupId {
        self.target_group
    }

    pub fn get_local_ids(&self) -> Vec<(u64, u64)> {
        let mut res = Vec::new();
        for entry in self.local_ids.iter() {
            res.push((*entry.key(), *entry.value()));
        }
        res
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
                        for (session_id, local_bkpt_id) in group_subbkpt.get_local_ids() {
                            get_bkpt_mgr().remove_local_bkpt_id_index(session_id, local_bkpt_id);
                        }
                        get_bkpt_mgr().delete_grp_bkpt(group_subbkpt.target_group, self.id);
                    }
                    SubBkptType::Session(sess_subbkpt) => {
                        get_bkpt_mgr().remove_local_bkpt_id_index(
                            sess_subbkpt.get_target_session(),
                            sess_subbkpt.get_local_bkpt_id(),
                        );
                    }
                }
                false
            }
        });
    }

    fn remove_all_subbkpts(&mut self) {
        for sb in &self.subbkpts {
            match &sb.subbkpt_type {
                SubBkptType::Group(group_subbkpt) => {
                    for (session_id, local_bkpt_id) in group_subbkpt.get_local_ids() {
                        get_bkpt_mgr().remove_local_bkpt_id_index(session_id, local_bkpt_id);
                    }
                    get_bkpt_mgr().delete_grp_bkpt(group_subbkpt.target_group, self.id);
                }
                SubBkptType::Session(sess_subbkpt) => {
                    get_bkpt_mgr().remove_local_bkpt_id_index(
                        sess_subbkpt.get_target_session(),
                        sess_subbkpt.get_local_bkpt_id(),
                    );
                }
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
    
    pub fn is_empty(&self) -> bool {
        self.subbkpts.is_empty()
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
    
    pub fn delete_local_bkpt(&mut self, subbkpt_id: u64, sid: u64) {
        self.subbkpts.retain_mut(|subbkpt| {
            if subbkpt.id == subbkpt_id {
                match &subbkpt.subbkpt_type {
                    SubBkptType::Group(group_subbkpt) => {
                        group_subbkpt.remove_local_bkpt(sid);
                        true
                    }
                    SubBkptType::Session(_) => {
                        false 
                    }
                }
            } else {
                true
            }
        });
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

    pub fn is_bkpt_empty(&self, bkpt_id: u64) -> Option<bool> {
        // Return None when there is no bkpt
        // Return Some(true) when the bkpt has no subbkpts
        // Return Some(false) when the bkpt has subbkpts
        if let Some(bkpt_entry) = self.bkpts.get(&bkpt_id) {
            Some(bkpt_entry.value().is_empty())
        } else {
            None
        }
    }

    pub fn add_bkpt(&self, loc: BkptLoc) -> u64 {
        let bkpt = BkptMeta::new(loc);
        let bkpt_id = bkpt.id;
        match self.bkpts.insert(bkpt_id, bkpt) {
            Some(_) => panic!("Breakpoint ID collision on {}!", bkpt_id),
            None => bkpt_id,
        }
    }

    pub fn delete_bkpt(&self, bkpt_id: u64) {
        if let Some((_, mut bkpt)) = self.bkpts.remove(&bkpt_id) {
            bkpt.remove_all_subbkpts();
        }
    }

    pub fn add_subbkpt(&self, bkpt_id: u64, subbkpt_type: SubBkptType) {
        if let Some(mut bkpt_entry) = self.bkpts.get_mut(&bkpt_id) {
            bkpt_entry.value_mut().add_subbkpt(subbkpt_type);
        }
    }

    pub fn get_subbkpt(&self, bkpt_id: u64, sub_bkpt_id: u64) -> Option<SubBkptMeta> {
        if let Some(bkpt_entry) = self.bkpts.get(&bkpt_id) {
            for sb in &bkpt_entry.value().subbkpts {
                if sb.id == sub_bkpt_id {
                    return Some(sb.clone());
                }
            }
        }
        None
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

    pub fn remove_local_bkpt_id_index(&self, session_id: SessionId, local_bkpt_id: u64) {
        self.local_bkpt_to_global
            .remove(&(session_id, local_bkpt_id));
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

    pub fn update_subbkpts_with<F: FnMut(&mut SubBkptMeta)>(&self, bkpt_id: u64, mut f: F) {
        self.bkpts.entry(bkpt_id).and_modify(|bkpt| {
            for sb in bkpt.subbkpts.iter_mut() {
                f(sb);
            }
        });
    }

    pub fn get_bkpt_by_id(&self, bkpt_id: u64) -> Option<BkptMeta> {
        self.bkpts.get(&bkpt_id).map(|entry| entry.value().clone())
    }

    pub fn get_bkpt_by_id_ref(
        &self,
        bkpt_id: u64,
    ) -> Option<dashmap::mapref::one::Ref<'_, u64, BkptMeta>> {
        self.bkpts.get(&bkpt_id)
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

    pub fn setup_grp_bkpt_for_new_session(
        &self,
        bkpt_id: u64,
        grp_id: GroupId,
        sid: SessionId,
        local_bkpt_id: u64,
    ) {
        let mut updated = false;
        self.update_subbkpts_with(bkpt_id, |subbkpt| {
            match &mut subbkpt.subbkpt_type {
                SubBkptType::Group(group_subbkpt) => {
                    if group_subbkpt.target_group == grp_id {
                        group_subbkpt.add_local_bkpt(sid, local_bkpt_id);
                        // register the local bkpt id index
                        self.insert_local_bkpt_id_index(sid, local_bkpt_id, bkpt_id, subbkpt.id);
                        updated = true;
                    }
                }
                _ => {}
            }
        });
        if updated {
            if let Some(bkpt) = self.get_bkpt_by_id(bkpt_id) {
                let out = MIFormatter::format("=", "breakpoint-modified", Some(&bkpt.into()), None);
                println!("{}", out);
                debug!("output: {}", out);
            } else {
                warn!(
                    "Failed to find bkpt {} when setting up group bkpt for new session",
                    bkpt_id
                );
            }
        }
    }

    pub fn clean_bkpts_for_terminated_session(&self, sid: SessionId) {
        let grp_id = get_group_mgr().get_grp_id_by_sid(sid);
        let bkpt_ids: Vec<u64> = self.bkpts.iter().map(|entry| *entry.key()).collect();
        for bkpt_id in bkpt_ids {
            let mut updated = false;
            let mut should_remove_bkpt = false;
            if let Some(mut bkpt_entry) = self.bkpts.get_mut(&bkpt_id) {
                let bkpt = bkpt_entry.value_mut();
                bkpt.subbkpts
                    .retain_mut(|subbkpt| match &mut subbkpt.subbkpt_type {
                        SubBkptType::Session(sess_subbkpt) => {
                            if sess_subbkpt.target_session == sid {
                                updated = true;
                                false
                            } else {
                                true
                            }
                        }
                        SubBkptType::Group(group_subbkpt) => {
                            let match_group =
                                grp_id.map_or(true, |gid| group_subbkpt.target_group == gid);
                            if match_group && group_subbkpt.remove_local_bkpt(sid).is_some() {
                                updated = true;
                            }
                            true
                        }
                    });
                if bkpt.subbkpts.is_empty() {
                    should_remove_bkpt = true;
                }
            }
            if should_remove_bkpt {
                self.delete_bkpt(bkpt_id);
                let out = MIFormatter::format(
                    "=",
                    "breakpoint-deleted",
                    Some(&bkpt_deleted_payload(bkpt_id)),
                    None,
                );
                println!("{}", out);
                debug!("output: {}", out);
                continue;
            }
            if updated {
                if let Some(bkpt) = self.get_bkpt_by_id(bkpt_id) {
                    let out =
                        MIFormatter::format("=", "breakpoint-modified", Some(&bkpt.into()), None);
                    println!("{}", out);
                    debug!("output: {}", out);
                } else {
                    warn!(
                        "Failed to find bkpt {} when cleaning bkpts for terminated session",
                        bkpt_id
                    );
                }
            }
        }

        let mut local_keys = Vec::new();
        for entry in self.local_bkpt_to_global.iter() {
            let ((sess_id, local_bkpt_id), _) = entry.pair();
            if *sess_id == sid {
                local_keys.push((*sess_id, *local_bkpt_id));
            }
        }
        for key in local_keys {
            self.local_bkpt_to_global.remove(&key);
        }
    }
    
    pub fn get_bkpt_ids_by_local_bkpt_id(
        &self,
        sid: SessionId,
        local_bkpt_id: u64,
    ) -> Option<(u64, u64)> {
        self.local_bkpt_to_global
            .get(&(sid, local_bkpt_id))
            .map(|entry| *entry.value())
    }
    
    pub fn delete_local_bkpt(&self, sid: SessionId, local_bkpt_id: u64) {
        if let Some((_, (bkpt_id, subbkpt_id))) = self.local_bkpt_to_global.remove(&(sid, local_bkpt_id)) {
            self.bkpts.entry(bkpt_id).and_modify(|bkpt| {
                bkpt.delete_local_bkpt(subbkpt_id, sid);
            });
            
            if let Some(bkpt) = self.get_bkpt_by_id(bkpt_id) {
                if bkpt.is_empty() {
                    // bkpt becomes empty, so here it is removed.
                    let out = MIFormatter::format(
                        "=",
                        "breakpoint-deleted",
                        Some(&bkpt_deleted_payload(bkpt.get_id())),
                        None,
                    );
                    println!("{}", out);
                    debug!("output: {}", out);
                    self.bkpts.remove(&bkpt_id);
                    return;
                }
                // bkpt is still not empty, but it has changed.
                let out = MIFormatter::format("=", "breakpoint-modified", Some(&bkpt.into()), None);
                println!("{}", out);
                debug!("output: {}", out);
            }
        }
    }
}
