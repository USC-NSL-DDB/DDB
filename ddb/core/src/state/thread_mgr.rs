use std::{
    collections::HashMap,
    sync::{RwLock, RwLockReadGuard},
};

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct LocalThreadId(pub u64, pub u64); // session id, thread id

impl LocalThreadId {
    #[inline]
    pub fn new(sid: u64, tid: u64) -> Self {
        Self(sid, tid)
    }

    #[inline]
    pub fn session_id(&self) -> u64 {
        self.0
    }

    #[inline]
    pub fn thread_id(&self) -> u64 {
        self.1
    }

    #[inline]
    pub fn into_parts(self) -> (u64, u64) {
        (self.0, self.1)
    }
}

impl From<LocalThreadId> for (u64, u64) {
    fn from(ltid: LocalThreadId) -> Self {
        (ltid.0, ltid.1)
    }
}

impl From<&LocalThreadId> for (u64, u64) {
    fn from(ltid: &LocalThreadId) -> Self {
        (ltid.0, ltid.1)
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct LocalThreadGroupId(pub u64, pub String);

impl LocalThreadGroupId {
    #[inline]
    pub fn new(sid: u64, tgid: &str) -> Self {
        Self(sid, tgid.to_string())
    }

    #[inline]
    pub fn session_id(&self) -> u64 {
        self.0
    }

    #[inline]
    pub fn thread_group_id(&self) -> &str {
        &self.1
    }

    #[inline]
    pub fn into_parts(self) -> (u64, String) {
        (self.0, self.1)
    }
}

impl From<LocalThreadGroupId> for (u64, String) {
    fn from(ltgid: LocalThreadGroupId) -> Self {
        (ltgid.0, ltgid.1)
    }
}

impl From<&LocalThreadGroupId> for (u64, String) {
    fn from(ltgid: &LocalThreadGroupId) -> Self {
        (ltgid.0, ltgid.1.clone())
    }
}

#[derive(Default)]
struct ThreadIndexes {
    ltid_to_gtid: HashMap<LocalThreadId, u64>,
    gtid_to_ltid: HashMap<u64, LocalThreadId>,
    ltgid_to_gtgid: HashMap<LocalThreadGroupId, u64>,
    gtgid_to_ltgid: HashMap<u64, LocalThreadGroupId>,
}

/// Consistent read view over thread and thread-group identifiers.
///
/// Callers use this for short synchronous projections so one logical read does
/// not repeatedly acquire the index lock. The view must not cross an await.
pub(crate) struct ThreadIdView<'a> {
    indexes: RwLockReadGuard<'a, ThreadIndexes>,
}

impl ThreadIdView<'_> {
    pub(crate) fn global_thread_id(&self, sid: u64, tid: u64) -> Option<u64> {
        self.indexes
            .ltid_to_gtid
            .get(&LocalThreadId::new(sid, tid))
            .copied()
    }

    pub(crate) fn global_thread_group_id(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.indexes
            .ltgid_to_gtgid
            .get(&LocalThreadGroupId::new(sid, tgid))
            .copied()
    }
}

#[allow(unused)]
pub struct ThreadStateMgr {
    // All four maps form two bidirectional indexes and must change together.
    indexes: RwLock<ThreadIndexes>,
}

impl ThreadStateMgr {
    pub fn new() -> Self {
        Self {
            indexes: RwLock::new(ThreadIndexes::default()),
        }
    }

    pub(crate) fn read_ids(&self) -> ThreadIdView<'_> {
        ThreadIdView {
            indexes: self.indexes.read().unwrap(),
        }
    }

    pub fn global_thread_id(&self, local_tid: &LocalThreadId) -> Option<u64> {
        self.indexes
            .read()
            .unwrap()
            .ltid_to_gtid
            .get(local_tid)
            .copied()
    }

    pub fn local_thread_id(&self, gtid: u64) -> Option<LocalThreadId> {
        self.indexes
            .read()
            .unwrap()
            .gtid_to_ltid
            .get(&gtid)
            .cloned()
    }

    pub fn global_thread_group_id(&self, local_tgid: &LocalThreadGroupId) -> Option<u64> {
        self.indexes
            .read()
            .unwrap()
            .ltgid_to_gtgid
            .get(local_tgid)
            .copied()
    }

    pub fn local_thread_group_id(&self, gtgid: u64) -> Option<LocalThreadGroupId> {
        self.indexes
            .read()
            .unwrap()
            .gtgid_to_ltgid
            .get(&gtgid)
            .cloned()
    }

    pub fn insert_thread(&self, local_tid: &LocalThreadId, gtid: u64) {
        let mut indexes = self.indexes.write().unwrap();
        if let Some(old_gtid) = indexes.ltid_to_gtid.insert(local_tid.clone(), gtid) {
            indexes.gtid_to_ltid.remove(&old_gtid);
        }
        if let Some(old_local_tid) = indexes.gtid_to_ltid.insert(gtid, local_tid.clone()) {
            indexes.ltid_to_gtid.remove(&old_local_tid);
        }
    }

    pub fn insert_thread_group(&self, local_tgid: &LocalThreadGroupId, gtgid: u64) {
        let mut indexes = self.indexes.write().unwrap();
        if let Some(old_gtgid) = indexes.ltgid_to_gtgid.insert(local_tgid.clone(), gtgid) {
            indexes.gtgid_to_ltgid.remove(&old_gtgid);
        }
        if let Some(old_local_tgid) = indexes.gtgid_to_ltgid.insert(gtgid, local_tgid.clone()) {
            indexes.ltgid_to_gtgid.remove(&old_local_tgid);
        }
    }

    pub fn get_or_insert_thread_with<F>(&self, local_tid: &LocalThreadId, create: F) -> u64
    where
        F: FnOnce() -> u64,
    {
        let mut indexes = self.indexes.write().unwrap();
        if let Some(gtid) = indexes.ltid_to_gtid.get(local_tid) {
            return *gtid;
        }
        let gtid = create();
        indexes.ltid_to_gtid.insert(local_tid.clone(), gtid);
        indexes.gtid_to_ltid.insert(gtid, local_tid.clone());
        gtid
    }

    pub fn get_or_insert_thread_group_with<F>(
        &self,
        local_tgid: &LocalThreadGroupId,
        create: F,
    ) -> u64
    where
        F: FnOnce() -> u64,
    {
        let mut indexes = self.indexes.write().unwrap();
        if let Some(gtgid) = indexes.ltgid_to_gtgid.get(local_tgid) {
            return *gtgid;
        }
        let gtgid = create();
        indexes.ltgid_to_gtgid.insert(local_tgid.clone(), gtgid);
        indexes.gtgid_to_ltgid.insert(gtgid, local_tgid.clone());
        gtgid
    }

    pub fn global_thread_ids_for_session(&self, sid: u64) -> Vec<u64> {
        self.indexes
            .read()
            .unwrap()
            .ltid_to_gtid
            .iter()
            .filter_map(|(local_tid, gtid)| (local_tid.session_id() == sid).then_some(*gtid))
            .collect()
    }

    pub fn remove_session(&self, sid: u64) {
        let mut indexes = self.indexes.write().unwrap();
        indexes
            .ltid_to_gtid
            .retain(|local_tid, _| local_tid.session_id() != sid);
        indexes
            .gtid_to_ltid
            .retain(|_, local_tid| local_tid.session_id() != sid);
        indexes
            .ltgid_to_gtgid
            .retain(|local_tgid, _| local_tgid.session_id() != sid);
        indexes
            .gtgid_to_ltgid
            .retain(|_, local_tgid| local_tgid.session_id() != sid);
    }

    pub fn remove_thread(&self, local_tid: &LocalThreadId) -> Option<u64> {
        let mut indexes = self.indexes.write().unwrap();
        let gtid = indexes.ltid_to_gtid.remove(local_tid)?;
        indexes.gtid_to_ltid.remove(&gtid);
        Some(gtid)
    }

    pub fn remove_thread_group(&self, local_tgid: &LocalThreadGroupId) -> Option<u64> {
        let mut indexes = self.indexes.write().unwrap();
        let gtgid = indexes.ltgid_to_gtgid.remove(local_tgid)?;
        indexes.gtgid_to_ltgid.remove(&gtgid);
        Some(gtgid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_thread_ids_support_tuple_conversion_and_accessors() {
        let ltid = LocalThreadId::new(7, 9);
        let ltgid = LocalThreadGroupId::new(7, "i1");
        let ltid_owned: (u64, u64) = ltid.clone().into();
        let ltid_ref: (u64, u64) = (&ltid).into();
        let ltgid_owned: (u64, String) = ltgid.clone().into();
        let ltgid_ref: (u64, String) = (&ltgid).into();

        assert_eq!(ltid_owned, (7, 9));
        assert_eq!(ltid_ref, (7, 9));
        assert_eq!(ltid.session_id(), 7);
        assert_eq!(ltid.thread_id(), 9);
        assert_eq!(ltid.0, 7);
        assert_eq!(ltid.1, 9);

        assert_eq!(ltgid_owned, (7, "i1".to_string()));
        assert_eq!(ltgid_ref, (7, "i1".to_string()));
        assert_eq!(ltgid.session_id(), 7);
        assert_eq!(ltgid.thread_group_id(), "i1");
        assert_eq!(ltgid.0, 7);
        assert_eq!(ltgid.1, "i1");
    }

    #[test]
    fn thread_state_manager_tracks_bidirectional_thread_and_group_indexes() {
        let mgr = ThreadStateMgr::new();
        let ltid_a = LocalThreadId::new(1, 10);
        let ltid_b = LocalThreadId::new(1, 11);
        let ltid_c = LocalThreadId::new(2, 20);
        let ltgid = LocalThreadGroupId::new(1, "i1");

        mgr.insert_thread(&ltid_a, 100);
        mgr.insert_thread(&ltid_b, 101);
        mgr.insert_thread(&ltid_c, 200);
        mgr.insert_thread_group(&ltgid, 300);

        assert_eq!(mgr.global_thread_id(&ltid_a), Some(100));
        assert_eq!(mgr.local_thread_id(101), Some(ltid_b.clone()));
        assert_eq!(mgr.global_thread_group_id(&ltgid), Some(300));
        assert_eq!(mgr.local_thread_group_id(300), Some(ltgid.clone()));

        let mut gtids = mgr.global_thread_ids_for_session(1);
        gtids.sort_unstable();
        assert_eq!(gtids, vec![100, 101]);

        assert_eq!(mgr.remove_thread(&ltid_a), Some(100));
        assert!(mgr.global_thread_id(&ltid_a).is_none());
        assert!(mgr.local_thread_id(100).is_none());

        assert_eq!(mgr.remove_thread_group(&ltgid), Some(300));
        assert!(mgr.global_thread_group_id(&ltgid).is_none());
        assert!(mgr.local_thread_group_id(300).is_none());
    }

    #[test]
    fn replacing_thread_mapping_preserves_the_bijection() {
        let mgr = ThreadStateMgr::new();
        let first = LocalThreadId::new(1, 10);
        let second = LocalThreadId::new(1, 11);

        mgr.insert_thread(&first, 100);
        mgr.insert_thread(&first, 101);
        assert_eq!(mgr.local_thread_id(100), None);

        mgr.insert_thread(&second, 101);
        assert_eq!(mgr.global_thread_id(&first), None);
        assert_eq!(mgr.local_thread_id(101), Some(second));
    }

    #[test]
    fn removing_session_clears_all_thread_and_group_indexes_for_that_session() {
        let mgr = ThreadStateMgr::new();
        let ltid_a = LocalThreadId::new(1, 10);
        let ltid_b = LocalThreadId::new(1, 11);
        let ltid_c = LocalThreadId::new(2, 20);
        let ltgid_a = LocalThreadGroupId::new(1, "i1");
        let ltgid_b = LocalThreadGroupId::new(2, "i2");

        mgr.insert_thread(&ltid_a, 100);
        mgr.insert_thread(&ltid_b, 101);
        mgr.insert_thread(&ltid_c, 200);
        mgr.insert_thread_group(&ltgid_a, 300);
        mgr.insert_thread_group(&ltgid_b, 400);

        mgr.remove_session(1);

        assert!(mgr.global_thread_id(&ltid_a).is_none());
        assert!(mgr.global_thread_id(&ltid_b).is_none());
        assert!(mgr.local_thread_id(100).is_none());
        assert!(mgr.local_thread_id(101).is_none());
        assert!(mgr.global_thread_group_id(&ltgid_a).is_none());
        assert!(mgr.local_thread_group_id(300).is_none());

        assert_eq!(mgr.global_thread_id(&ltid_c), Some(200));
        assert_eq!(mgr.global_thread_group_id(&ltgid_b), Some(400));
    }
}
