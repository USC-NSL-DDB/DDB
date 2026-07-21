use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    sync::RwLock,
};

use crate::common::counter::SimpleCounter;

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
}

#[derive(Default)]
struct GroupIndexes {
    hash_to_group: HashMap<GroupHash, GroupMeta>,
    id_to_hash: HashMap<GroupId, GroupHash>,
    sid_to_hash: HashMap<SessionId, GroupHash>,
}

pub struct GroupMgr {
    // These maps describe one relation and must never be observed half-updated.
    indexes: RwLock<GroupIndexes>,
    ids: SimpleCounter,
}

impl Debug for GroupMgr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let indexes = self.indexes.read().unwrap();
        f.debug_struct("GroupMgr")
            .field("hash_to_groups", &indexes.hash_to_group)
            .field("id_to_hash", &indexes.id_to_hash)
            .field("sid_to_group", &indexes.sid_to_hash)
            .finish()
    }
}

impl GroupMgr {
    pub fn new() -> Self {
        Self {
            indexes: RwLock::new(GroupIndexes::default()),
            ids: SimpleCounter::new(),
        }
    }

    fn remove_session_locked(indexes: &mut GroupIndexes, sid: SessionId) {
        let Some(group_hash) = indexes.sid_to_hash.remove(&sid) else {
            return;
        };
        let remove_group_id = indexes
            .hash_to_group
            .get_mut(&group_hash)
            .and_then(|group| {
                group.remove_session(sid);
                group.session_ids().is_empty().then_some(group.id())
            });
        if let Some(group_id) = remove_group_id {
            indexes.hash_to_group.remove(&group_hash);
            indexes.id_to_hash.remove(&group_id);
        }
    }

    #[inline]
    pub fn register_session(&self, group_hash: &str, alias: String, sid: SessionId) {
        let mut indexes = self.indexes.write().unwrap();
        if indexes
            .sid_to_hash
            .get(&sid)
            .is_some_and(|current_hash| current_hash != group_hash)
        {
            Self::remove_session_locked(&mut indexes, sid);
        }

        let group_hash = group_hash.to_string();
        if !indexes.hash_to_group.contains_key(&group_hash) {
            let group_id = self.ids.next();
            indexes.id_to_hash.insert(group_id, group_hash.clone());
            indexes.hash_to_group.insert(
                group_hash.clone(),
                GroupMeta::new(group_id, group_hash.clone(), alias),
            );
        }
        indexes.sid_to_hash.insert(sid, group_hash.clone());
        indexes
            .hash_to_group
            .get_mut(&group_hash)
            .expect("group inserted above")
            .add_session(sid);
    }

    #[inline]
    pub fn remove_session(&self, sid: SessionId) {
        Self::remove_session_locked(&mut self.indexes.write().unwrap(), sid);
    }

    #[inline]
    pub fn group_info_by_session(&self, sid: SessionId) -> Option<(GroupId, GroupHash)> {
        let indexes = self.indexes.read().unwrap();
        let hash = indexes.sid_to_hash.get(&sid)?;
        let group_id = indexes.hash_to_group.get(hash)?.id();
        Some((group_id, hash.clone()))
    }

    #[inline]
    pub fn group_hash_by_session(&self, sid: SessionId) -> Option<GroupHash> {
        self.indexes.read().unwrap().sid_to_hash.get(&sid).cloned()
    }

    #[inline]
    pub fn group_id_by_session(&self, sid: SessionId) -> Option<GroupId> {
        self.group_info_by_session(sid)
            .map(|(group_id, _)| group_id)
    }

    #[inline]
    pub fn group_by_hash(&self, hash: &str) -> Option<GroupMeta> {
        self.indexes
            .read()
            .unwrap()
            .hash_to_group
            .get(hash)
            .cloned()
    }

    #[inline]
    pub fn group_by_id(&self, id: GroupId) -> Option<GroupMeta> {
        let indexes = self.indexes.read().unwrap();
        let hash = indexes.id_to_hash.get(&id)?;
        indexes.hash_to_group.get(hash).cloned()
    }

    #[inline]
    pub fn groups(&self) -> Vec<GroupMeta> {
        self.indexes
            .read()
            .unwrap()
            .hash_to_group
            .values()
            .cloned()
            .collect()
    }

    #[inline]
    pub fn matching_groups<P>(&self, predicate: P) -> Vec<GroupMeta>
    where
        P: Fn(&GroupMeta) -> bool,
    {
        self.groups()
            .into_iter()
            .filter(|group| predicate(group))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_session_reuses_existing_group_for_same_hash() {
        let mgr = GroupMgr::new();

        mgr.register_session("hash-a", "svc-a".to_string(), 11);
        mgr.register_session("hash-a", "svc-b".to_string(), 12);

        let group = mgr
            .group_by_hash("hash-a")
            .expect("group should exist for hash");
        let group_id = group.id();

        assert_eq!(group.session_ids().len(), 2);
        assert!(group.session_ids().contains(&11));
        assert!(group.session_ids().contains(&12));
        assert_eq!(mgr.group_id_by_session(11), Some(group_id));
        assert_eq!(mgr.group_id_by_session(12), Some(group_id));
    }

    #[test]
    fn registering_session_in_new_group_removes_old_membership() {
        let mgr = GroupMgr::new();
        mgr.register_session("hash-a", "svc-a".to_string(), 11);
        let old_group_id = mgr.group_id_by_session(11).unwrap();

        mgr.register_session("hash-b", "svc-b".to_string(), 11);

        assert!(mgr.group_by_id(old_group_id).is_none());
        assert!(mgr.group_by_hash("hash-a").is_none());
        assert_eq!(mgr.group_hash_by_session(11), Some("hash-b".to_string()));
    }

    #[test]
    fn remove_session_drops_empty_group_indexes() {
        let mgr = GroupMgr::new();

        mgr.register_session("hash-a", "svc-a".to_string(), 11);
        let group_id = mgr
            .group_id_by_session(11)
            .expect("session should be indexed");

        mgr.remove_session(11);

        assert!(mgr.group_hash_by_session(11).is_none());
        assert!(mgr.group_by_hash("hash-a").is_none());
        assert!(mgr.group_by_id(group_id).is_none());
        assert!(mgr.groups().is_empty());
    }
}
