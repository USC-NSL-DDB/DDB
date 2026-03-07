use std::ops::Deref;

use dashmap::DashMap;

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct LocalThreadId(pub u64, pub u64); // session id, thread id

impl LocalThreadId {
    pub fn new(sid: u64, tid: u64) -> Self {
        Self(sid, tid)
    }
}

impl From<LocalThreadId> for (u64, u64) {
    fn from(ltid: LocalThreadId) -> (u64, u64) {
        (ltid.0, ltid.1)
    }
}

impl From<&LocalThreadId> for (u64, u64) {
    fn from(ltid: &LocalThreadId) -> (u64, u64) {
        (ltid.0, ltid.1)
    }
}

impl Deref for LocalThreadId {
    type Target = (u64, u64);

    fn deref(&self) -> &Self::Target {
        unsafe { &*(self as *const LocalThreadId as *const (u64, u64)) }
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct LocalThreadGroupId(pub u64, pub String);

impl LocalThreadGroupId {
    pub fn new(sid: u64, tgid: &str) -> Self {
        Self(sid, tgid.to_string())
    }
}

impl From<LocalThreadGroupId> for (u64, String) {
    fn from(ltgid: LocalThreadGroupId) -> (u64, String) {
        (ltgid.0, ltgid.1)
    }
}

impl From<&LocalThreadGroupId> for (u64, String) {
    fn from(ltgid: &LocalThreadGroupId) -> (u64, String) {
        (ltgid.0, ltgid.1.clone())
    }
}

impl Deref for LocalThreadGroupId {
    type Target = (u64, String);

    fn deref(&self) -> &Self::Target {
        unsafe { &*(self as *const LocalThreadGroupId as *const (u64, String)) }
    }
}

#[allow(unused)]
pub struct ThreadStateMgr {
    // local thread id (session id + thread id) to global thread id
    ltid_to_gtid: DashMap<LocalThreadId, u64>,
    // global thread id to local thread id (session id + thread id)
    gtid_to_ltid: DashMap<u64, LocalThreadId>,

    // local thread group id (session id + thread group id) to global thread group id
    ltgid_to_gtgid: DashMap<LocalThreadGroupId, u64>,
    // global thread group id to local thread group id (session id + thread group id)
    gtgid_to_ltgid: DashMap<u64, LocalThreadGroupId>,
}

impl ThreadStateMgr {
    pub fn new() -> Self {
        Self {
            ltid_to_gtid: DashMap::new(),
            gtid_to_ltid: DashMap::new(),
            ltgid_to_gtgid: DashMap::new(),
            gtgid_to_ltgid: DashMap::new(),
        }
    }

    pub fn get_gtid(&self, ltid: &LocalThreadId) -> Option<u64> {
        self.ltid_to_gtid.get(ltid).map(|v| *v)
    }

    pub fn get_ltid(&self, gtid: u64) -> Option<LocalThreadId> {
        self.gtid_to_ltid.get(&gtid).map(|v| v.clone())
    }

    pub fn get_gtgid(&self, ltgid: &LocalThreadGroupId) -> Option<u64> {
        self.ltgid_to_gtgid.get(ltgid).map(|v| *v)
    }

    #[allow(unused)]
    pub fn get_ltgid(&self, gtgid: u64) -> Option<LocalThreadGroupId> {
        self.gtgid_to_ltgid.get(&gtgid).map(|v| v.clone())
    }

    pub fn insert_tid(&self, ltid: &LocalThreadId, gtid: u64) {
        self.ltid_to_gtid.insert(ltid.clone(), gtid);
        self.gtid_to_ltid.insert(gtid, ltid.clone());
    }

    pub fn insert_tgid(&self, ltgid: &LocalThreadGroupId, gtgid: u64) {
        self.ltgid_to_gtgid.insert(ltgid.clone(), gtgid);
        self.gtgid_to_ltgid.insert(gtgid, ltgid.clone());
    }

    pub fn get_gtids_by_sid(&self, sid: u64) -> Vec<u64> {
        self.ltid_to_gtid
            .iter()
            .filter_map(|v| {
                if v.key().0 == sid {
                    Some(*v.value())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn remove_by_ltid(&self, ltid: &LocalThreadId) -> Option<u64> {
        if let Some(gtid) = self.get_gtid(ltid) {
            self.ltid_to_gtid.remove(ltid);
            self.gtid_to_ltid.remove(&gtid);
            return Some(gtid);
        }
        None
    }

    pub fn remove_by_ltgid(&self, ltgid: &LocalThreadGroupId) -> Option<u64> {
        if let Some(gtgid) = self.get_gtgid(ltgid) {
            self.ltgid_to_gtgid.remove(ltgid);
            self.gtgid_to_ltgid.remove(&gtgid);
            return Some(gtgid);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_thread_ids_support_tuple_conversion_and_deref() {
        let ltid = LocalThreadId::new(7, 9);
        let ltgid = LocalThreadGroupId::new(7, "i1");
        let ltid_owned: (u64, u64) = ltid.clone().into();
        let ltid_ref: (u64, u64) = (&ltid).into();
        let ltgid_owned: (u64, String) = ltgid.clone().into();
        let ltgid_ref: (u64, String) = (&ltgid).into();

        assert_eq!(ltid_owned, (7, 9));
        assert_eq!(ltid_ref, (7, 9));
        assert_eq!(ltid.0, 7);
        assert_eq!(ltid.1, 9);

        assert_eq!(ltgid_owned, (7, "i1".to_string()));
        assert_eq!(ltgid_ref, (7, "i1".to_string()));
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

        mgr.insert_tid(&ltid_a, 100);
        mgr.insert_tid(&ltid_b, 101);
        mgr.insert_tid(&ltid_c, 200);
        mgr.insert_tgid(&ltgid, 300);

        assert_eq!(mgr.get_gtid(&ltid_a), Some(100));
        assert_eq!(mgr.get_ltid(101), Some(ltid_b.clone()));
        assert_eq!(mgr.get_gtgid(&ltgid), Some(300));
        assert_eq!(mgr.get_ltgid(300), Some(ltgid.clone()));

        let mut gtids = mgr.get_gtids_by_sid(1);
        gtids.sort_unstable();
        assert_eq!(gtids, vec![100, 101]);

        assert_eq!(mgr.remove_by_ltid(&ltid_a), Some(100));
        assert!(mgr.get_gtid(&ltid_a).is_none());
        assert!(mgr.get_ltid(100).is_none());

        assert_eq!(mgr.remove_by_ltgid(&ltgid), Some(300));
        assert!(mgr.get_gtgid(&ltgid).is_none());
        assert!(mgr.get_ltgid(300).is_none());
    }
}
