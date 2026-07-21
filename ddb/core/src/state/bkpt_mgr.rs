use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc, RwLock,
    },
};

use super::GroupId;
use crate::{common::counter::SimpleCounter, state::SessionId};

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
    pub fn new(src: impl Into<String>, line: u64) -> Self {
        Self {
            src: src.into(),
            line,
        }
    }

    pub fn path(&self) -> &str {
        &self.src
    }

    pub fn line(&self) -> u64 {
        self.line
    }

    pub fn breakpoint_path(&self) -> String {
        format!("{}:{}", self.src, self.line)
    }
}

impl From<[&str; 2]> for BkptLoc {
    fn from(arr: [&str; 2]) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        Self::new(src, line)
    }
}

impl From<&[&str; 2]> for BkptLoc {
    fn from(arr: &[&str; 2]) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        Self::new(src, line)
    }
}

impl From<Vec<&str>> for BkptLoc {
    fn from(arr: Vec<&str>) -> Self {
        let src = arr[0].to_string();
        let line = arr[1].parse::<u64>().unwrap_or(0);
        Self::new(src, line)
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

    pub fn target_session(&self) -> u64 {
        self.target_session
    }

    pub fn local_id(&self) -> u64 {
        self.local_id
    }
}

#[derive(Debug, Clone)]
pub struct GroupSubBkpt {
    // map from session_id to local_bkpt_id
    local_ids: HashMap<u64, u64>,
    target_group: GroupId,
}

impl GroupSubBkpt {
    pub fn new(target_group: GroupId) -> Self {
        GroupSubBkpt {
            local_ids: HashMap::new(),
            target_group,
        }
    }

    pub fn add_local_bkpt(&mut self, session_id: u64, local_bkpt_id: u64) {
        self.local_ids.insert(session_id, local_bkpt_id);
    }

    pub fn remove_local_bkpt(&mut self, session_id: u64) -> Option<u64> {
        self.local_ids.remove(&session_id)
    }

    pub fn target_group(&self) -> GroupId {
        self.target_group
    }

    pub fn local_ids(&self) -> Vec<(u64, u64)> {
        self.local_ids
            .iter()
            .map(|(session_id, local_id)| (*session_id, *local_id))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.local_ids.is_empty()
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
        SubBkptMeta {
            id,
            major_bkpt_id,
            subbkpt_type,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn kind(&self) -> &SubBkptType {
        &self.subbkpt_type
    }

    fn local_ids(&self) -> Vec<(SessionId, u64)> {
        match &self.subbkpt_type {
            SubBkptType::Session(sess_subbkpt) => {
                vec![(sess_subbkpt.target_session(), sess_subbkpt.local_id())]
            }
            SubBkptType::Group(group_subbkpt) => group_subbkpt.local_ids(),
        }
    }

    fn group_target(&self) -> Option<GroupId> {
        match &self.subbkpt_type {
            SubBkptType::Session(_) => None,
            SubBkptType::Group(group_subbkpt) => Some(group_subbkpt.target_group()),
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
    fn new(id: u64, loc: BkptLoc) -> Self {
        let bkpt_id = id;
        BkptMeta {
            id: bkpt_id,
            subbkpts: Vec::new(),
            loc: loc,
            enabled: Arc::new(AtomicBool::new(true)),
            times: Arc::new(AtomicU64::new(0)),
            sub_bkpt_counter: Arc::new(SimpleCounter::new()),
        }
    }

    fn add_subbkpt(&mut self, subbkpt_type: SubBkptType) -> SubBkptMeta {
        let subbkpt_id = self.sub_bkpt_counter.next();
        let subbkpt = SubBkptMeta::new(subbkpt_id, self.id, subbkpt_type);
        self.subbkpts.push(subbkpt.clone());
        subbkpt
    }

    fn delete_subbkpt(&mut self, subbkpt_id: u64) -> Option<SubBkptMeta> {
        let index = self
            .subbkpts
            .iter()
            .position(|subbkpt| subbkpt.id == subbkpt_id)?;
        Some(self.subbkpts.remove(index))
    }

    fn remove_all_subbkpts(&mut self) -> Vec<SubBkptMeta> {
        std::mem::take(&mut self.subbkpts)
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

    pub fn times(&self) -> u64 {
        self.times.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn location(&self) -> &BkptLoc {
        &self.loc
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn sub_breakpoints(&self) -> &[SubBkptMeta] {
        &self.subbkpts
    }

    pub fn is_empty(&self) -> bool {
        self.subbkpts.is_empty()
    }

    fn delete_local_bkpt(&mut self, subbkpt_id: u64, sid: u64) -> Option<SubBkptMeta> {
        let Some(index) = self
            .subbkpts
            .iter()
            .position(|subbkpt| subbkpt.id == subbkpt_id)
        else {
            return None;
        };

        let remove_subbkpt = match &mut self.subbkpts[index].subbkpt_type {
            SubBkptType::Group(group_subbkpt) => {
                group_subbkpt.remove_local_bkpt(sid);
                group_subbkpt.is_empty()
            }
            SubBkptType::Session(_) => true,
        };

        if remove_subbkpt {
            Some(self.subbkpts.remove(index))
        } else {
            None
        }
    }

    fn remove_session_targets(
        &mut self,
        sid: SessionId,
        grp_id: Option<GroupId>,
    ) -> (bool, Vec<SubBkptMeta>) {
        let mut updated = false;
        let mut removed = Vec::new();
        let mut index = 0;
        while index < self.subbkpts.len() {
            let remove = match &mut self.subbkpts[index].subbkpt_type {
                SubBkptType::Session(sess_subbkpt) => {
                    if sess_subbkpt.target_session == sid {
                        updated = true;
                        true
                    } else {
                        false
                    }
                }
                SubBkptType::Group(group_subbkpt) => {
                    let belongs_to_group =
                        grp_id.is_none_or(|group_id| group_subbkpt.target_group == group_id);
                    if belongs_to_group && group_subbkpt.remove_local_bkpt(sid).is_some() {
                        updated = true;
                    }
                    belongs_to_group && group_subbkpt.is_empty()
                }
            };
            if remove {
                removed.push(self.subbkpts.remove(index));
            } else {
                index += 1;
            }
        }
        (updated, removed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BreakpointStateChange {
    None,
    TargetChanged(u64),
    Removed(u64),
}

#[derive(Debug)]
struct BreakpointState {
    bkpts: HashMap<u64, BkptMeta>,
    local_bkpt_to_global: HashMap<(SessionId, u64), (u64, u64)>,
    group_bkpts: HashMap<GroupId, HashSet<u64>>,
}

#[derive(Debug)]
pub struct BreakpointMgr {
    // The primary store and both reverse indexes are one consistency unit.
    state: RwLock<BreakpointState>,
    ids: SimpleCounter,
}

impl BreakpointMgr {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(BreakpointState {
                bkpts: HashMap::new(),
                local_bkpt_to_global: HashMap::new(),
                group_bkpts: HashMap::new(),
            }),
            ids: SimpleCounter::new(),
        }
    }

    pub fn breakpoint_is_empty(&self, bkpt_id: u64) -> Option<bool> {
        // Return None when there is no bkpt
        // Return Some(true) when the bkpt has no subbkpts
        // Return Some(false) when the bkpt has subbkpts
        self.state
            .read()
            .unwrap()
            .bkpts
            .get(&bkpt_id)
            .map(BkptMeta::is_empty)
    }

    pub fn add_breakpoint(&self, loc: BkptLoc) -> u64 {
        let bkpt = BkptMeta::new(self.ids.next(), loc);
        let bkpt_id = bkpt.id;
        match self.state.write().unwrap().bkpts.insert(bkpt_id, bkpt) {
            Some(_) => panic!("Breakpoint ID collision on {}!", bkpt_id),
            None => bkpt_id,
        }
    }

    pub fn remove_breakpoint(&self, bkpt_id: u64) {
        let mut state = self.state.write().unwrap();
        if let Some(mut bkpt) = state.bkpts.remove(&bkpt_id) {
            for subbkpt in bkpt.remove_all_subbkpts() {
                Self::unregister_subbkpt(&mut state, &subbkpt);
            }
        }
    }

    pub fn add_sub_breakpoint(&self, bkpt_id: u64, subbkpt_type: SubBkptType) {
        let mut state = self.state.write().unwrap();
        if let Some(subbkpt) = state
            .bkpts
            .get_mut(&bkpt_id)
            .map(|bkpt| bkpt.add_subbkpt(subbkpt_type))
        {
            Self::register_subbkpt(&mut state, &subbkpt);
        }
    }

    pub fn sub_breakpoint(&self, bkpt_id: u64, sub_bkpt_id: u64) -> Option<SubBkptMeta> {
        self.state
            .read()
            .unwrap()
            .bkpts
            .get(&bkpt_id)?
            .sub_breakpoints()
            .iter()
            .find(|subbkpt| subbkpt.id == sub_bkpt_id)
            .cloned()
    }

    pub fn remove_sub_breakpoint(&self, bkpt_id: u64, subbkpt_id: u64) {
        let mut state = self.state.write().unwrap();
        if let Some(subbkpt) = state
            .bkpts
            .get_mut(&bkpt_id)
            .and_then(|bkpt| bkpt.delete_subbkpt(subbkpt_id))
        {
            Self::unregister_subbkpt(&mut state, &subbkpt);
        }
    }

    fn add_group_bkpt(state: &mut BreakpointState, group_id: GroupId, bkpt_id: u64) {
        state
            .group_bkpts
            .entry(group_id)
            .or_default()
            .insert(bkpt_id);
    }

    fn delete_group_bkpt(state: &mut BreakpointState, group_id: GroupId, bkpt_id: u64) {
        if let Some(bkpt_ids) = state.group_bkpts.get_mut(&group_id) {
            bkpt_ids.remove(&bkpt_id);
            if bkpt_ids.is_empty() {
                state.group_bkpts.remove(&group_id);
            }
        }
    }

    fn insert_local_bkpt_id_index(
        state: &mut BreakpointState,
        session_id: SessionId,
        local_bkpt_id: u64,
        major_bkpt_id: u64,
        sub_bkpt_id: u64,
    ) {
        state
            .local_bkpt_to_global
            .insert((session_id, local_bkpt_id), (major_bkpt_id, sub_bkpt_id));
    }

    fn remove_local_bkpt_id_index(
        state: &mut BreakpointState,
        session_id: SessionId,
        local_bkpt_id: u64,
    ) {
        state
            .local_bkpt_to_global
            .remove(&(session_id, local_bkpt_id));
    }

    pub fn group_breakpoints(&self, grp_id: GroupId) -> Vec<BkptMeta> {
        let state = self.state.read().unwrap();
        state
            .group_bkpts
            .get(&grp_id)
            .map(|bkpt_ids| {
                bkpt_ids
                    .iter()
                    .filter_map(|bkpt_id| state.bkpts.get(bkpt_id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn group_breakpoint_locations(&self, grp_id: GroupId) -> Vec<BkptLoc> {
        let state = self.state.read().unwrap();
        state
            .group_bkpts
            .get(&grp_id)
            .map(|bkpt_ids| {
                bkpt_ids
                    .iter()
                    .filter_map(|bkpt_id| state.bkpts.get(bkpt_id).map(|bkpt| bkpt.loc.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn breakpoint(&self, bkpt_id: u64) -> Option<BkptMeta> {
        self.state.read().unwrap().bkpts.get(&bkpt_id).cloned()
    }

    pub fn breakpoints(&self) -> Vec<BkptMeta> {
        self.state.read().unwrap().bkpts.values().cloned().collect()
    }

    pub fn local_breakpoint_ids(&self, bkpt_id: u64) -> Vec<(SessionId, u64)> {
        self.state
            .read()
            .unwrap()
            .local_bkpt_to_global
            .iter()
            .filter_map(|((sess_id, local_bkpt_id), (global_bkpt_id, _))| {
                (*global_bkpt_id == bkpt_id).then_some((*sess_id, *local_bkpt_id))
            })
            .collect()
    }

    pub fn attach_group_breakpoint_session_target(
        &self,
        bkpt_id: u64,
        grp_id: GroupId,
        sid: SessionId,
        local_bkpt_id: u64,
    ) -> BreakpointStateChange {
        let updated = {
            let mut state = self.state.write().unwrap();
            let subbkpt_ids = if let Some(bkpt) = state.bkpts.get_mut(&bkpt_id) {
                let mut subbkpt_ids = Vec::new();
                for subbkpt in bkpt.subbkpts.iter_mut() {
                    if let SubBkptType::Group(group_subbkpt) = &mut subbkpt.subbkpt_type {
                        if group_subbkpt.target_group == grp_id {
                            group_subbkpt.add_local_bkpt(sid, local_bkpt_id);
                            subbkpt_ids.push(subbkpt.id);
                        }
                    }
                }
                subbkpt_ids
            } else {
                Vec::new()
            };
            for subbkpt_id in &subbkpt_ids {
                Self::insert_local_bkpt_id_index(
                    &mut state,
                    sid,
                    local_bkpt_id,
                    bkpt_id,
                    *subbkpt_id,
                );
            }
            !subbkpt_ids.is_empty()
        };

        if updated {
            BreakpointStateChange::TargetChanged(bkpt_id)
        } else {
            BreakpointStateChange::None
        }
    }

    pub fn clean_bkpts_for_terminated_session(
        &self,
        sid: SessionId,
        grp_id: Option<GroupId>,
    ) -> Vec<BreakpointStateChange> {
        {
            let mut state = self.state.write().unwrap();
            let bkpt_ids = state.bkpts.keys().copied().collect::<Vec<_>>();
            let mut changes = Vec::new();
            for bkpt_id in bkpt_ids {
                let (updated, removed, is_empty) = match state.bkpts.get_mut(&bkpt_id) {
                    Some(bkpt) => {
                        let (updated, removed) = bkpt.remove_session_targets(sid, grp_id);
                        (updated, removed, bkpt.is_empty())
                    }
                    None => continue,
                };
                for subbkpt in removed {
                    Self::unregister_subbkpt(&mut state, &subbkpt);
                }
                if is_empty {
                    state.bkpts.remove(&bkpt_id);
                    changes.push(BreakpointStateChange::Removed(bkpt_id));
                } else if updated {
                    changes.push(BreakpointStateChange::TargetChanged(bkpt_id));
                }
            }
            state
                .local_bkpt_to_global
                .retain(|(session_id, _), _| *session_id != sid);
            changes
        }
    }

    pub fn breakpoint_ids_by_local_id(
        &self,
        sid: SessionId,
        local_bkpt_id: u64,
    ) -> Option<(u64, u64)> {
        self.state
            .read()
            .unwrap()
            .local_bkpt_to_global
            .get(&(sid, local_bkpt_id))
            .copied()
    }

    pub fn record_local_bkpt_deletion(
        &self,
        sid: SessionId,
        local_bkpt_id: u64,
    ) -> BreakpointStateChange {
        let mut state = self.state.write().unwrap();
        let Some((bkpt_id, subbkpt_id)) = state.local_bkpt_to_global.remove(&(sid, local_bkpt_id))
        else {
            return BreakpointStateChange::None;
        };

        let (removed_subbkpt, is_empty) = match state.bkpts.get_mut(&bkpt_id) {
            Some(bkpt) => {
                let removed = bkpt.delete_local_bkpt(subbkpt_id, sid);
                (removed, bkpt.is_empty())
            }
            None => return BreakpointStateChange::None,
        };
        if let Some(subbkpt) = removed_subbkpt {
            Self::unregister_subbkpt(&mut state, &subbkpt);
        }

        if is_empty {
            state.bkpts.remove(&bkpt_id);
            BreakpointStateChange::Removed(bkpt_id)
        } else {
            BreakpointStateChange::TargetChanged(bkpt_id)
        }
    }

    fn register_subbkpt(state: &mut BreakpointState, subbkpt: &SubBkptMeta) {
        for (session_id, local_bkpt_id) in subbkpt.local_ids() {
            Self::insert_local_bkpt_id_index(
                state,
                session_id,
                local_bkpt_id,
                subbkpt.major_bkpt_id,
                subbkpt.id,
            );
        }

        if let Some(group_id) = subbkpt.group_target() {
            Self::add_group_bkpt(state, group_id, subbkpt.major_bkpt_id);
        }
    }

    fn unregister_subbkpt(state: &mut BreakpointState, subbkpt: &SubBkptMeta) {
        for (session_id, local_bkpt_id) in subbkpt.local_ids() {
            Self::remove_local_bkpt_id_index(state, session_id, local_bkpt_id);
        }

        if let Some(group_id) = subbkpt.group_target() {
            let still_targets_group = state.bkpts.get(&subbkpt.major_bkpt_id).is_some_and(|bkpt| {
                bkpt.sub_breakpoints()
                    .iter()
                    .any(|remaining| remaining.group_target() == Some(group_id))
            });
            if !still_targets_group {
                Self::delete_group_bkpt(state, group_id, subbkpt.major_bkpt_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_and_deleting_group_subbreakpoints_keeps_indexes_consistent() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_breakpoint(BkptLoc::from(["main.rs", "10"]));

        let mut group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        group_subbkpt.add_local_bkpt(12, 202);

        mgr.add_sub_breakpoint(bkpt_id, SubBkptType::Group(group_subbkpt));

        let mut local_ids = mgr.local_breakpoint_ids(bkpt_id);
        local_ids.sort_unstable();
        assert_eq!(local_ids, vec![(11, 101), (12, 202)]);
        assert_eq!(mgr.group_breakpoints(7).len(), 1);

        mgr.remove_breakpoint(bkpt_id);

        assert!(mgr.local_breakpoint_ids(bkpt_id).is_empty());
        assert!(mgr.group_breakpoints(7).is_empty());
        assert!(!mgr.state.read().unwrap().group_bkpts.contains_key(&7));
        assert!(mgr.breakpoint(bkpt_id).is_none());
    }

    #[test]
    fn deleting_session_subbreakpoint_unregisters_local_index() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_breakpoint(BkptLoc::from(["main.rs", "10"]));

        mgr.add_sub_breakpoint(bkpt_id, SubBkptType::Session(SessionSubBkpt::new(55, 3)));

        let subbkpt_id = mgr
            .breakpoint(bkpt_id)
            .and_then(|bkpt| bkpt.sub_breakpoints().first().map(SubBkptMeta::id))
            .expect("sub-breakpoint should exist");

        assert_eq!(
            mgr.breakpoint_ids_by_local_id(3, 55),
            Some((bkpt_id, subbkpt_id))
        );

        mgr.remove_sub_breakpoint(bkpt_id, subbkpt_id);

        assert_eq!(mgr.breakpoint_ids_by_local_id(3, 55), None);
        assert_eq!(mgr.breakpoint_is_empty(bkpt_id), Some(true));
    }

    #[test]
    fn recording_local_breakpoint_deletion_keeps_group_breakpoint_until_last_target() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_breakpoint(BkptLoc::from(["main.rs", "10"]));

        let mut group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        group_subbkpt.add_local_bkpt(12, 202);
        mgr.add_sub_breakpoint(bkpt_id, SubBkptType::Group(group_subbkpt));

        assert_eq!(
            mgr.record_local_bkpt_deletion(11, 101),
            BreakpointStateChange::TargetChanged(bkpt_id)
        );
        assert!(mgr.breakpoint(bkpt_id).is_some());
        assert_eq!(mgr.breakpoint_ids_by_local_id(11, 101), None);
        assert_eq!(
            mgr.breakpoint_ids_by_local_id(12, 202).map(|ids| ids.0),
            Some(bkpt_id)
        );
    }

    #[test]
    fn recording_last_local_breakpoint_deletion_removes_group_breakpoint() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_breakpoint(BkptLoc::from(["main.rs", "10"]));

        let mut group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        mgr.add_sub_breakpoint(bkpt_id, SubBkptType::Group(group_subbkpt));

        assert_eq!(
            mgr.record_local_bkpt_deletion(11, 101),
            BreakpointStateChange::Removed(bkpt_id)
        );
        assert!(mgr.breakpoint(bkpt_id).is_none());
        assert_eq!(mgr.breakpoint_ids_by_local_id(11, 101), None);
        assert!(mgr.group_breakpoints(7).is_empty());
        assert!(!mgr.state.read().unwrap().group_bkpts.contains_key(&7));
    }

    #[test]
    fn attaching_group_target_returns_change_without_publishing() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_breakpoint(BkptLoc::from(["main.rs", "10"]));
        mgr.add_sub_breakpoint(bkpt_id, SubBkptType::Group(GroupSubBkpt::new(7)));

        assert_eq!(
            mgr.attach_group_breakpoint_session_target(bkpt_id, 7, 11, 101),
            BreakpointStateChange::TargetChanged(bkpt_id)
        );
        assert_eq!(
            mgr.breakpoint_ids_by_local_id(11, 101).map(|ids| ids.0),
            Some(bkpt_id)
        );
        assert_eq!(
            mgr.attach_group_breakpoint_session_target(bkpt_id, 8, 12, 202),
            BreakpointStateChange::None
        );
    }

    #[test]
    fn cleaning_session_returns_all_state_changes_and_keeps_indexes_consistent() {
        let mgr = BreakpointMgr::new();

        let retained_bkpt_id = mgr.add_breakpoint(BkptLoc::from(["main.rs", "10"]));
        let mut group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        group_subbkpt.add_local_bkpt(12, 202);
        mgr.add_sub_breakpoint(retained_bkpt_id, SubBkptType::Group(group_subbkpt));

        let removed_bkpt_id = mgr.add_breakpoint(BkptLoc::from(["worker.rs", "20"]));
        mgr.add_sub_breakpoint(
            removed_bkpt_id,
            SubBkptType::Session(SessionSubBkpt::new(303, 11)),
        );

        let changes = mgr.clean_bkpts_for_terminated_session(11, Some(7));

        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&BreakpointStateChange::TargetChanged(retained_bkpt_id)));
        assert!(changes.contains(&BreakpointStateChange::Removed(removed_bkpt_id)));
        assert_eq!(mgr.breakpoint_ids_by_local_id(11, 101), None);
        assert_eq!(mgr.breakpoint_ids_by_local_id(11, 303), None);
        assert_eq!(
            mgr.breakpoint_ids_by_local_id(12, 202).map(|ids| ids.0),
            Some(retained_bkpt_id)
        );
        assert!(mgr.breakpoint(retained_bkpt_id).is_some());
        assert!(mgr.breakpoint(removed_bkpt_id).is_none());
    }
}
