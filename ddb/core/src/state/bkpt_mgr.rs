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
    notification::{get_notif_mgr, BreakpointChangeEvent, Notification, NotificationPayload},
    state::SessionId,
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
        self.local_ids
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
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

    pub fn get_id(&self) -> u64 {
        self.id
    }

    pub fn get_type(&self) -> &SubBkptType {
        &self.subbkpt_type
    }

    fn get_local_ids(&self) -> Vec<(SessionId, u64)> {
        match &self.subbkpt_type {
            SubBkptType::Session(sess_subbkpt) => {
                vec![(
                    sess_subbkpt.get_target_session(),
                    sess_subbkpt.get_local_bkpt_id(),
                )]
            }
            SubBkptType::Group(group_subbkpt) => group_subbkpt.get_local_ids(),
        }
    }

    fn get_target_group(&self) -> Option<GroupId> {
        match &self.subbkpt_type {
            SubBkptType::Session(_) => None,
            SubBkptType::Group(group_subbkpt) => Some(group_subbkpt.get_target_group()),
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

    pub fn delete_local_bkpt(&mut self, subbkpt_id: u64, sid: u64) {
        let Some(index) = self
            .subbkpts
            .iter()
            .position(|subbkpt| subbkpt.id == subbkpt_id)
        else {
            return;
        };

        let remove_subbkpt = match &self.subbkpts[index].subbkpt_type {
            SubBkptType::Group(group_subbkpt) => {
                group_subbkpt.remove_local_bkpt(sid);
                group_subbkpt.is_empty()
            }
            SubBkptType::Session(_) => true,
        };

        if remove_subbkpt {
            self.subbkpts.remove(index);
        }
    }

    fn remove_session_targets(&mut self, sid: SessionId, grp_id: Option<GroupId>) -> bool {
        let mut updated = false;
        self.subbkpts
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
                    let belongs_to_group =
                        grp_id.is_none_or(|group_id| group_subbkpt.target_group == group_id);
                    if belongs_to_group && group_subbkpt.remove_local_bkpt(sid).is_some() {
                        updated = true;
                    }
                    true
                }
            });
        updated
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BreakpointMutation {
    None,
    TargetChanged,
    Removed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BreakpointStateChange {
    None,
    TargetChanged(u64),
    Removed(u64),
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
        self.with_bkpt(bkpt_id, |bkpt| bkpt.is_empty())
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
            for subbkpt in bkpt.remove_all_subbkpts() {
                self.unregister_subbkpt(&subbkpt);
            }
        }
    }

    pub fn add_subbkpt(&self, bkpt_id: u64, subbkpt_type: SubBkptType) {
        if let Some(subbkpt) = self.with_bkpt_mut(bkpt_id, |bkpt| bkpt.add_subbkpt(subbkpt_type)) {
            self.register_subbkpt(&subbkpt);
        }
    }

    pub fn get_subbkpt(&self, bkpt_id: u64, sub_bkpt_id: u64) -> Option<SubBkptMeta> {
        self.with_bkpt(bkpt_id, |bkpt| {
            bkpt.get_subbkpts()
                .iter()
                .find(|subbkpt| subbkpt.id == sub_bkpt_id)
                .cloned()
        })
        .flatten()
    }

    pub fn delete_subbkpt(&self, bkpt_id: u64, subbkpt_id: u64) {
        if let Some(Some(subbkpt)) =
            self.with_bkpt_mut(bkpt_id, |bkpt| bkpt.delete_subbkpt(subbkpt_id))
        {
            self.unregister_subbkpt(&subbkpt);
        }
    }

    fn add_grp_bkpt(&self, grp_id: GroupId, bkpt_id: u64) {
        self.group_bkpt.entry(grp_id).or_default().insert(bkpt_id);
    }

    fn delete_grp_bkpt(&self, grp_id: GroupId, bkpt_id: u64) {
        if let Some(mut entry) = self.group_bkpt.get_mut(&grp_id) {
            entry.value_mut().remove(&bkpt_id);
            if entry.is_empty() {
                drop(entry);
                self.group_bkpt.remove(&grp_id);
            }
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

    fn remove_local_bkpt_id_index(&self, session_id: SessionId, local_bkpt_id: u64) {
        self.local_bkpt_to_global
            .remove(&(session_id, local_bkpt_id));
    }

    pub fn get_bkpts_by_grp_id(&self, grp_id: GroupId) -> Vec<BkptMeta> {
        self.group_bkpt
            .get(&grp_id)
            .map(|entry| {
                entry
                    .iter()
                    .filter_map(|bkpt_id| self.get_bkpt_by_id(*bkpt_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_bkpt_locs_by_grp_id(&self, grp_id: GroupId) -> Vec<BkptLoc> {
        self.group_bkpt
            .get(&grp_id)
            .map(|entry| {
                entry
                    .iter()
                    .filter_map(|bkpt_id| self.with_bkpt(*bkpt_id, |bkpt| bkpt.loc.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_bkpt_by_id(&self, bkpt_id: u64) -> Option<BkptMeta> {
        self.bkpts.get(&bkpt_id).map(|entry| entry.value().clone())
    }

    pub fn get_all_bkpts(&self) -> Vec<BkptMeta> {
        self.bkpts
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn get_local_bkpt_ids(&self, bkpt_id: u64) -> Vec<(SessionId, u64)> {
        self.local_bkpt_to_global
            .iter()
            .filter_map(|entry| {
                let ((sess_id, local_bkpt_id), (global_bkpt_id, _)) = entry.pair();
                (*global_bkpt_id == bkpt_id).then_some((*sess_id, *local_bkpt_id))
            })
            .collect()
    }

    pub async fn setup_grp_bkpt_for_new_session(
        &self,
        bkpt_id: u64,
        grp_id: GroupId,
        sid: SessionId,
        local_bkpt_id: u64,
    ) {
        let updated = self
            .with_bkpt_mut(bkpt_id, |bkpt| {
                let mut updated = false;
                for subbkpt in bkpt.subbkpts.iter_mut() {
                    if let SubBkptType::Group(group_subbkpt) = &mut subbkpt.subbkpt_type {
                        if group_subbkpt.target_group == grp_id {
                            group_subbkpt.add_local_bkpt(sid, local_bkpt_id);
                            self.insert_local_bkpt_id_index(
                                sid,
                                local_bkpt_id,
                                bkpt_id,
                                subbkpt.id,
                            );
                            updated = true;
                        }
                    }
                }
                updated
            })
            .unwrap_or(false);
        if updated {
            self.emit_target_changed(bkpt_id, "setting up group bkpt for new session")
                .await;
        }
    }

    pub async fn clean_bkpts_for_terminated_session(&self, sid: SessionId) {
        let grp_id = get_group_mgr().group_id_by_session(sid);
        let bkpt_ids: Vec<u64> = self.bkpts.iter().map(|entry| *entry.key()).collect();
        for bkpt_id in bkpt_ids {
            match self.remove_session_targets(bkpt_id, sid, grp_id) {
                BreakpointMutation::Removed => {
                    self.delete_bkpt(bkpt_id);
                    self.emit_removed(bkpt_id).await;
                }
                BreakpointMutation::TargetChanged => {
                    self.emit_target_changed(bkpt_id, "cleaning bkpts for terminated session")
                        .await;
                }
                BreakpointMutation::None => {}
            }
        }

        self.remove_session_local_indexes(sid);
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

    pub fn record_local_bkpt_deletion(
        &self,
        sid: SessionId,
        local_bkpt_id: u64,
    ) -> BreakpointStateChange {
        let Some((_, (bkpt_id, subbkpt_id))) =
            self.local_bkpt_to_global.remove(&(sid, local_bkpt_id))
        else {
            return BreakpointStateChange::None;
        };

        let mutation = self
            .with_bkpt_mut(bkpt_id, |bkpt| {
                bkpt.delete_local_bkpt(subbkpt_id, sid);
                if bkpt.is_empty() {
                    BreakpointMutation::Removed
                } else {
                    BreakpointMutation::TargetChanged
                }
            })
            .unwrap_or(BreakpointMutation::None);

        match mutation {
            BreakpointMutation::Removed => {
                self.delete_bkpt(bkpt_id);
                BreakpointStateChange::Removed(bkpt_id)
            }
            BreakpointMutation::TargetChanged => BreakpointStateChange::TargetChanged(bkpt_id),
            BreakpointMutation::None => BreakpointStateChange::None,
        }
    }

    pub async fn delete_local_bkpt(&self, sid: SessionId, local_bkpt_id: u64) {
        match self.record_local_bkpt_deletion(sid, local_bkpt_id) {
            BreakpointStateChange::Removed(bkpt_id) => {
                self.emit_removed(bkpt_id).await;
            }
            BreakpointStateChange::TargetChanged(bkpt_id) => {
                self.emit_target_changed(bkpt_id, "deleting local breakpoint")
                    .await;
            }
            BreakpointStateChange::None => {}
        }
    }

    #[inline]
    fn with_bkpt<U, F>(&self, bkpt_id: u64, f: F) -> Option<U>
    where
        F: FnOnce(&BkptMeta) -> U,
    {
        let bkpt = self.bkpts.get(&bkpt_id)?;
        Some(f(bkpt.value()))
    }

    #[inline]
    fn with_bkpt_mut<U, F>(&self, bkpt_id: u64, f: F) -> Option<U>
    where
        F: FnOnce(&mut BkptMeta) -> U,
    {
        let mut bkpt = self.bkpts.get_mut(&bkpt_id)?;
        Some(f(bkpt.value_mut()))
    }

    fn register_subbkpt(&self, subbkpt: &SubBkptMeta) {
        for (session_id, local_bkpt_id) in subbkpt.get_local_ids() {
            self.insert_local_bkpt_id_index(
                session_id,
                local_bkpt_id,
                subbkpt.major_bkpt_id,
                subbkpt.id,
            );
        }

        if let Some(group_id) = subbkpt.get_target_group() {
            self.add_grp_bkpt(group_id, subbkpt.major_bkpt_id);
        }
    }

    fn unregister_subbkpt(&self, subbkpt: &SubBkptMeta) {
        for (session_id, local_bkpt_id) in subbkpt.get_local_ids() {
            self.remove_local_bkpt_id_index(session_id, local_bkpt_id);
        }

        if let Some(group_id) = subbkpt.get_target_group() {
            self.delete_grp_bkpt(group_id, subbkpt.major_bkpt_id);
        }
    }

    fn remove_session_targets(
        &self,
        bkpt_id: u64,
        sid: SessionId,
        grp_id: Option<GroupId>,
    ) -> BreakpointMutation {
        self.with_bkpt_mut(bkpt_id, |bkpt| {
            let updated = bkpt.remove_session_targets(sid, grp_id);
            if bkpt.is_empty() {
                BreakpointMutation::Removed
            } else if updated {
                BreakpointMutation::TargetChanged
            } else {
                BreakpointMutation::None
            }
        })
        .unwrap_or(BreakpointMutation::None)
    }

    fn remove_session_local_indexes(&self, sid: SessionId) {
        let local_keys: Vec<_> = self
            .local_bkpt_to_global
            .iter()
            .filter_map(|entry| {
                let ((sess_id, local_bkpt_id), _) = entry.pair();
                (*sess_id == sid).then_some((*sess_id, *local_bkpt_id))
            })
            .collect();

        for key in local_keys {
            self.local_bkpt_to_global.remove(&key);
        }
    }

    async fn emit_target_changed(&self, bkpt_id: u64, context: &str) {
        if let Some(bkpt) = self.get_bkpt_by_id(bkpt_id) {
            let out = MIFormatter::format("=", "breakpoint-modified", Some(&bkpt.into()), None);
            println!("{}", out);
            debug!("output: {}", out);

            get_notif_mgr()
                .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                    BreakpointChangeEvent::TargetChanged(bkpt_id),
                )))
                .await;
        } else {
            warn!("Failed to find bkpt {} when {}", bkpt_id, context);
        }
    }

    async fn emit_removed(&self, bkpt_id: u64) {
        let out = MIFormatter::format(
            "=",
            "breakpoint-deleted",
            Some(&bkpt_deleted_payload(bkpt_id)),
            None,
        );
        println!("{}", out);
        debug!("output: {}", out);

        get_notif_mgr()
            .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                BreakpointChangeEvent::Removed(bkpt_id),
            )))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_and_deleting_group_subbreakpoints_keeps_indexes_consistent() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_bkpt(BkptLoc::from(["main.rs", "10"]));

        let group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        group_subbkpt.add_local_bkpt(12, 202);

        mgr.add_subbkpt(bkpt_id, SubBkptType::Group(group_subbkpt));

        let mut local_ids = mgr.get_local_bkpt_ids(bkpt_id);
        local_ids.sort_unstable();
        assert_eq!(local_ids, vec![(11, 101), (12, 202)]);
        assert_eq!(mgr.get_bkpts_by_grp_id(7).len(), 1);

        mgr.delete_bkpt(bkpt_id);

        assert!(mgr.get_local_bkpt_ids(bkpt_id).is_empty());
        assert!(mgr.get_bkpts_by_grp_id(7).is_empty());
        assert!(mgr.get_bkpt_by_id(bkpt_id).is_none());
    }

    #[test]
    fn deleting_session_subbreakpoint_unregisters_local_index() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_bkpt(BkptLoc::from(["main.rs", "10"]));

        mgr.add_subbkpt(bkpt_id, SubBkptType::Session(SessionSubBkpt::new(55, 3)));

        let subbkpt_id = mgr
            .get_bkpt_by_id(bkpt_id)
            .and_then(|bkpt| bkpt.get_subbkpts().first().map(SubBkptMeta::get_id))
            .expect("sub-breakpoint should exist");

        assert_eq!(
            mgr.get_bkpt_ids_by_local_bkpt_id(3, 55),
            Some((bkpt_id, subbkpt_id))
        );

        mgr.delete_subbkpt(bkpt_id, subbkpt_id);

        assert_eq!(mgr.get_bkpt_ids_by_local_bkpt_id(3, 55), None);
        assert_eq!(mgr.is_bkpt_empty(bkpt_id), Some(true));
    }

    #[test]
    fn recording_local_breakpoint_deletion_keeps_group_breakpoint_until_last_target() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_bkpt(BkptLoc::from(["main.rs", "10"]));

        let group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        group_subbkpt.add_local_bkpt(12, 202);
        mgr.add_subbkpt(bkpt_id, SubBkptType::Group(group_subbkpt));

        assert_eq!(
            mgr.record_local_bkpt_deletion(11, 101),
            BreakpointStateChange::TargetChanged(bkpt_id)
        );
        assert!(mgr.get_bkpt_by_id(bkpt_id).is_some());
        assert_eq!(mgr.get_bkpt_ids_by_local_bkpt_id(11, 101), None);
        assert_eq!(
            mgr.get_bkpt_ids_by_local_bkpt_id(12, 202).map(|ids| ids.0),
            Some(bkpt_id)
        );
    }

    #[test]
    fn recording_last_local_breakpoint_deletion_removes_group_breakpoint() {
        let mgr = BreakpointMgr::new();
        let bkpt_id = mgr.add_bkpt(BkptLoc::from(["main.rs", "10"]));

        let group_subbkpt = GroupSubBkpt::new(7);
        group_subbkpt.add_local_bkpt(11, 101);
        mgr.add_subbkpt(bkpt_id, SubBkptType::Group(group_subbkpt));

        assert_eq!(
            mgr.record_local_bkpt_deletion(11, 101),
            BreakpointStateChange::Removed(bkpt_id)
        );
        assert!(mgr.get_bkpt_by_id(bkpt_id).is_none());
        assert_eq!(mgr.get_bkpt_ids_by_local_bkpt_id(11, 101), None);
        assert!(mgr.get_bkpts_by_grp_id(7).is_empty());
    }
}
