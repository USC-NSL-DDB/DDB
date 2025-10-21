use dashmap::DashMap;
use serde::Serialize;
use std::{collections::HashSet, fmt::Debug};

pub type GroupId = String;
pub type SessionId = u64;

#[derive(Clone, Debug, Serialize)]
pub struct GroupMeta {
    alias: String,
    sids: HashSet<SessionId>,
}

impl GroupMeta {
    #[inline]
    pub fn new(alias: String) -> Self {
        Self {
            alias,
            sids: HashSet::new(),
        }
    }

    #[inline]
    pub fn insert(&mut self, sid: SessionId) {
        self.sids.insert(sid);
    }

    #[inline]
    pub fn remove(&mut self, sid: &SessionId) {
        self.sids.remove(sid);
    }

    pub fn get_alias(&self) -> &String {
        &self.alias
    }

    pub fn get_sids(&self) -> &HashSet<SessionId> {
        &self.sids
    }
}

pub struct GroupMgr {
    // maps from binary id (sha256 hash) to a set of dbg session ids
    groups: DashMap<GroupId, GroupMeta>,

    // a reverse index
    sid_to_group: DashMap<SessionId, GroupId>,
}

impl Debug for GroupMgr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupMgr")
            .field("groups", &self.groups)
            .field("sid_to_group", &self.sid_to_group)
            .finish()
    }
}

impl GroupMgr {
    pub fn new() -> Self {
        Self {
            groups: DashMap::new(),
            sid_to_group: DashMap::new(),
        }
    }

    #[inline]
    pub fn add_session(&self, bin_id: String, alias: String, sid: u64) {
        self.sid_to_group.insert(sid, bin_id.clone());
        self.groups
            .entry(bin_id.clone())
            .or_insert(GroupMeta::new(alias))
            .insert(sid);
    }

    #[inline]
    pub fn remove_session(&self, sid: u64) {
        if let Some((_, group_id)) = self.sid_to_group.remove(&sid) {
            self.groups.entry(group_id).and_modify(|s| {
                let _ = s.remove(&sid);
            });
        }
    }

    #[inline]
    pub fn get_group_id_by_sid(&self, sid: u64) -> Option<GroupId> {
        self.sid_to_group
            .get(&sid)
            .map(|s| s.value().clone())
    }

    #[inline]
    pub fn get_group(&self, bin_id: &String) -> Option<GroupMeta> {
        self.groups.get(bin_id).map(|s| s.clone())
    }

    #[inline]
    pub fn get_group_id(&self, sid: u64) -> Option<GroupId> {
        self.sid_to_group.get(&sid).map(|s| s.clone())
    }

    #[inline]
    pub fn get_all_group_meta(&self) -> Vec<GroupMeta> {
        self.groups.iter().map(|s| s.value().clone()).collect()
    }

    #[inline]
    pub fn get_all_group_meta_if<P>(&self, f: P) -> Vec<GroupMeta>
    where
        P: Fn(&GroupMeta) -> bool,
    {
        self.groups
            .iter()
            .filter_map(|s| {
                if f(s.value()) {
                    Some(s.value().clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[inline]
    pub fn get_all_groups_if<P>(&self, f: P) -> std::collections::HashMap<GroupId, GroupMeta>
    where
        P: Fn(&GroupId, &GroupMeta) -> bool,
    {
        self.groups
            .iter()
            .filter_map(|s| {
                if f(s.key(), s.value()) {
                    Some((s.key().clone(), s.value().clone()))
                } else {
                    None
                }
            })
            .collect()
    }
}
