use dashmap::DashMap;
use serde::Serialize;
use std::{collections::HashSet, fmt::Debug};

use crate::common::counter::next_group_id;

pub type GroupId = u64;
pub type GroupHash = String;
pub type SessionId = u64;

#[derive(Clone, Debug, Serialize)]
pub struct GroupMeta {
    id: GroupId,
    hash: GroupHash,
    alias: String,
    sids: HashSet<SessionId>,
}

impl GroupMeta {
    #[inline]
    pub fn new(id: GroupId, hash: GroupHash, alias: String) -> Self {
        Self {
            id,
            hash,
            alias,
            sids: HashSet::new(),
        }
    }

    #[inline]
    pub fn add_session(&mut self, sid: SessionId) {
        self.sids.insert(sid);
    }

    #[inline]
    pub fn remove_session(&mut self, sid: SessionId) {
        self.sids.remove(&sid);
    }

    #[inline]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    #[inline]
    pub fn session_ids(&self) -> &HashSet<SessionId> {
        &self.sids
    }

    #[inline]
    pub fn id(&self) -> GroupId {
        self.id
    }

    #[inline]
    pub fn hash(&self) -> &GroupHash {
        &self.hash
    }

    #[inline]
    pub fn insert(&mut self, sid: SessionId) {
        self.add_session(sid);
    }

    #[inline]
    pub fn remove(&mut self, sid: &SessionId) {
        self.remove_session(*sid);
    }

    #[inline]
    pub fn get_alias(&self) -> &String {
        &self.alias
    }

    #[inline]
    pub fn get_sids(&self) -> &HashSet<SessionId> {
        &self.sids
    }

    #[inline]
    pub fn get_grp_id(&self) -> GroupId {
        self.id()
    }

    #[inline]
    pub fn get_grp_hash(&self) -> &GroupHash {
        self.hash()
    }
}

pub struct GroupMgr {
    // maps from group hash (by default, when binary hash is used, this is a sha256 hash)
    // to a set of dbg session ids
    hash_to_grp: DashMap<GroupHash, GroupMeta>,

    // maps from group ID to group hash
    id_to_hash: DashMap<GroupId, GroupHash>,

    // a reverse index
    sid_to_grp: DashMap<SessionId, GroupHash>,
}

impl Debug for GroupMgr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupMgr")
            .field("hash_to_grps", &self.hash_to_grp)
            .field("id_to_hash", &self.id_to_hash)
            .field("sid_to_group", &self.sid_to_grp)
            .finish()
    }
}

impl GroupMgr {
    pub fn new() -> Self {
        Self {
            hash_to_grp: DashMap::new(),
            id_to_hash: DashMap::new(),
            sid_to_grp: DashMap::new(),
        }
    }

    #[inline]
    pub fn register_session(&self, group_hash: &str, alias: String, sid: SessionId) {
        self.sid_to_grp.insert(sid, group_hash.to_string());
        self.hash_to_grp
            .entry(group_hash.to_string())
            .or_insert_with(|| {
                let grp_id = next_group_id();
                self.id_to_hash.insert(grp_id, group_hash.to_string());
                GroupMeta::new(grp_id, group_hash.to_string(), alias)
            })
            .add_session(sid);
    }

    #[inline]
    pub fn remove_session(&self, sid: SessionId) {
        if let Some((_, group_hash)) = self.sid_to_grp.remove(&sid) {
            let remove_group_id = {
                let mut group = match self.hash_to_grp.get_mut(&group_hash) {
                    Some(group) => group,
                    None => return,
                };
                group.remove_session(sid);
                if group.session_ids().is_empty() {
                    Some(group.id())
                } else {
                    None
                }
            };

            if let Some(group_id) = remove_group_id {
                self.hash_to_grp.remove(&group_hash);
                self.id_to_hash.remove(&group_id);
            }
        }
    }

    #[inline]
    pub fn group_id_by_hash(&self, hash: &str) -> Option<GroupId> {
        self.hash_to_grp.get(hash).map(|s| s.id)
    }

    #[inline]
    pub fn group_info_by_session(&self, sid: SessionId) -> Option<(GroupId, GroupHash)> {
        if let Some(group_hash) = self.group_hash_by_session(sid) {
            if let Some(group_id) = self.group_id_by_session(sid) {
                return Some((group_id, group_hash));
            }
        }
        None
    }

    #[inline]
    pub fn group_hash_by_session(&self, sid: SessionId) -> Option<GroupHash> {
        self.sid_to_grp.get(&sid).map(|s| s.value().clone())
    }

    #[inline]
    pub fn group_id_by_session(&self, sid: SessionId) -> Option<GroupId> {
        if let Some(group_hash) = self.group_hash_by_session(sid) {
            self.group_id_by_hash(&group_hash)
        } else {
            None
        }
    }

    #[inline]
    pub fn group_by_hash(&self, hash: &str) -> Option<GroupMeta> {
        self.hash_to_grp.get(hash).map(|s| s.clone())
    }

    #[inline]
    pub fn group_by_id(&self, id: GroupId) -> Option<GroupMeta> {
        if let Some(hash) = self.id_to_hash.get(&id) {
            self.group_by_hash(&hash)
        } else {
            None
        }
    }

    #[inline]
    pub fn groups(&self) -> Vec<GroupMeta> {
        self.hash_to_grp.iter().map(|s| s.value().clone()).collect()
    }

    #[inline]
    pub fn matching_groups<P>(&self, f: P) -> Vec<GroupMeta>
    where
        P: Fn(&GroupMeta) -> bool,
    {
        self.hash_to_grp
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
    pub fn add_session(&self, bin_id: &str, alias: String, sid: u64) {
        self.register_session(bin_id, alias, sid);
    }

    #[inline]
    pub fn get_grp_id_by_hash(&self, hash: &str) -> Option<GroupId> {
        self.group_id_by_hash(hash)
    }

    #[inline]
    pub fn get_grp_info_by_sid(&self, sid: u64) -> Option<(GroupId, GroupHash)> {
        self.group_info_by_session(sid)
    }

    #[inline]
    pub fn get_grp_hash_by_sid(&self, sid: u64) -> Option<GroupHash> {
        self.group_hash_by_session(sid)
    }

    #[inline]
    pub fn get_grp_id_by_sid(&self, sid: u64) -> Option<GroupId> {
        self.group_id_by_session(sid)
    }

    #[inline]
    pub fn get_grp_by_hash(&self, hash: &str) -> Option<GroupMeta> {
        self.group_by_hash(hash)
    }

    #[inline]
    pub fn get_grp_by_id(&self, id: GroupId) -> Option<GroupMeta> {
        self.group_by_id(id)
    }

    #[inline]
    pub fn get_all_grp_meta(&self) -> Vec<GroupMeta> {
        self.groups()
    }

    #[inline]
    pub fn get_all_grp_meta_if<P>(&self, f: P) -> Vec<GroupMeta>
    where
        P: Fn(&GroupMeta) -> bool,
    {
        self.matching_groups(f)
    }

    #[inline]
    pub fn get_all_grps_if<P>(&self, f: P) -> std::collections::HashMap<GroupHash, GroupMeta>
    where
        P: Fn(&GroupHash, &GroupMeta) -> bool,
    {
        self.hash_to_grp
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_session_reuses_existing_group_for_same_hash() {
        let mgr = GroupMgr::new();

        mgr.add_session("hash-a", "svc-a".to_string(), 11);
        mgr.add_session("hash-a", "svc-b".to_string(), 12);

        let group = mgr
            .get_grp_by_hash("hash-a")
            .expect("group should exist for hash");
        let grp_id = group.get_grp_id();

        assert_eq!(group.get_sids().len(), 2);
        assert!(group.get_sids().contains(&11));
        assert!(group.get_sids().contains(&12));
        assert_eq!(mgr.get_grp_id_by_sid(11), Some(grp_id));
        assert_eq!(mgr.get_grp_id_by_sid(12), Some(grp_id));
    }

    #[test]
    fn remove_session_drops_empty_group_indexes() {
        let mgr = GroupMgr::new();

        mgr.add_session("hash-a", "svc-a".to_string(), 11);
        let grp_id = mgr
            .get_grp_id_by_sid(11)
            .expect("session should be indexed");

        mgr.remove_session(11);

        assert!(mgr.get_grp_hash_by_sid(11).is_none());
        assert!(mgr.get_grp_by_hash("hash-a").is_none());
        assert!(mgr.get_grp_by_id(grp_id).is_none());
        assert!(mgr.get_all_grp_meta().is_empty());
    }

    #[test]
    fn get_all_grps_if_filters_groups_using_predicate() {
        let mgr = GroupMgr::new();

        mgr.add_session("hash-a", "svc-a".to_string(), 11);
        mgr.add_session("hash-b", "svc-b".to_string(), 21);
        mgr.add_session("hash-b", "svc-b".to_string(), 22);

        let populated = mgr.get_all_grps_if(|_, grp| grp.get_sids().len() > 1);

        assert_eq!(populated.len(), 1);
        assert!(populated.contains_key("hash-b"));
        assert_eq!(populated["hash-b"].get_sids().len(), 2);
    }
}
