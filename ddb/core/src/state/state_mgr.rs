use std::sync::Mutex;

use crate::{common::counter, discovery::discovery_message_producer::ServiceMeta};

use super::{
    session_mgr,
    thread_mgr::{self, LocalThreadGroupId, LocalThreadId},
    SessionMetaRef,
};

pub struct StateMgr {
    session_states: session_mgr::SessionStateMgr,
    thread_states: thread_mgr::ThreadStateMgr,

    curr_session: Mutex<Option<u64>>,
    selected_gthread: Mutex<Option<u64>>,
}

#[allow(unused)]
impl StateMgr {
    pub fn new() -> Self {
        Self {
            session_states: session_mgr::SessionStateMgr::new(),
            thread_states: thread_mgr::ThreadStateMgr::new(),

            curr_session: Mutex::new(None),
            selected_gthread: Mutex::new(None),
        }
    }

    #[inline]
    pub async fn register_session(&self, sid: u64, tag: &str, service_meta: Option<ServiceMeta>) {
        self.session_states.add_session(sid, tag, service_meta).await;
    }

    #[inline]
    pub async fn remove_session(&self, sid: u64) {
        self.session_states.remove_session(sid).await;
    }

    #[inline]
    pub async fn update_session_status_on(&self, sid: u64) {
        self.session_states.update_session_status_on(sid).await;
    }

    #[inline]
    pub async fn update_session_status_off(&self, sid: u64) {
        self.session_states.update_session_status_off(sid).await;
    }

    #[inline]
    pub fn get_gtids_by_sid(&self, sid: u64) -> Vec<u64> {
        self.thread_states.get_gtids_by_sid(sid)
    }

    // Adds a thread group (process) to the state manager.
    //
    // Args:
    //     sid (int): The session ID.
    //     tgid (str): The thread group ID.
    //
    // Returns:
    //     int: The global inferior/process/thread group ID assigned to the thread group.
    #[inline]
    pub async fn add_thread_group(&self, sid: u64, tgid: &str) -> u64 {
        let gtgid = counter::next_g_inferior_id();
        self.thread_states
            .insert_tgid(&LocalThreadGroupId::new(sid, tgid), gtgid);
        self.session_states.add_thread_group(sid, tgid).await;
        gtgid
    }

    #[inline]
    pub async fn remove_thread_group(&self, sid: u64, tgid: &str) -> Option<u64> {
        let gtgid = self
            .thread_states
            .get_gtgid(&LocalThreadGroupId::new(sid, tgid));

        let tids = self.session_states.remove_thread_group(sid, tgid).await;
        for tid in tids {
            let ltid = LocalThreadId::new(sid, tid);
            self.thread_states.remove_by_ltid(&ltid);
        }

        self.thread_states
            .remove_by_ltgid(&LocalThreadGroupId::new(sid, tgid));
        gtgid
    }

    #[inline]
    pub async fn start_thread_group(&self, sid: u64, tgid: &str, pid: u64) -> Option<u64> {
        self.session_states.start_thread_group(sid, tgid, pid).await;
        self.thread_states
            .get_gtgid(&LocalThreadGroupId::new(sid, tgid))
    }

    #[inline]
    pub async fn exit_thread_group(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.session_states.exit_thread_group(sid, tgid).await;
        self.thread_states
            .get_gtgid(&LocalThreadGroupId::new(sid, tgid))
    }

    // Creates a new global thread in the state manager by mapping the session specific thread information.
    // Args:
    //     sid (int): The session ID from gdb/mi output.
    //     tid (int): The thread ID from gdb/mi output.
    //     tgid (str): The thread group ID from gdb/mi output.
    // Returns:
    //     int: The global thread ID assigned to the new thread.
    //     int: The global thread group id associated with this newly created thread.
    #[inline]
    pub async fn create_thread(&self, sid: u64, tid: u64, tgid: &str) -> (u64, u64) {
        let gtid = counter::next_g_thread_id();
        self.thread_states
            .insert_tid(&LocalThreadId::new(sid, tid), gtid);
        self.session_states.create_thread(sid, tid, tgid).await;
        let gtgid = self
            .thread_states
            .get_gtgid(&LocalThreadGroupId::new(sid, tgid))
            .unwrap();
        (gtid, gtgid)
    }

    #[inline]
    pub async fn update_thread_status(
        &self,
        sid: u64,
        tid: u64,
        status: session_mgr::ThreadStatus,
    ) {
        self.session_states.update_t_status(sid, tid, status).await;
    }

    #[inline]
    pub async fn update_all_thread_status(&self, sid: u64, status: session_mgr::ThreadStatus) {
        self.session_states.update_all_status(sid, status).await;
    }

    /// Note: This function sets global select thread id
    /// and also update the local selected thread id
    /// for the corresponding session.
    // pub fn set_curr_tid(&self, sid: u64, tid: u64) {
    //     self.session_states.set_curr_tid(sid, tid);
    //     let gtid = self.get_gtid(sid, tid).unwrap();
    //     self.selected_gthread.lock().unwrap().replace(gtid);
    // }

    /// Note: This function sets global select thread id
    /// and also update the local selected thread id
    /// for the corresponding session.
    #[inline]
    pub async fn set_curr_gtid(&self, gtid: u64) {
        let ltid = self.get_ltid_by_gtid(gtid).unwrap();
        self.set_curr_gtid_by_ltid(ltid.0, ltid.1).await;
    }

    /// Note: This function sets global select thread id
    /// and also update the local selected thread id
    /// for the corresponding session.
    #[inline]
    pub async fn set_curr_gtid_by_ltid(&self, sid: u64, tid: u64) {
        self.session_states.set_curr_tid(sid, tid).await;
        let gtid = self.get_gtid(sid, tid).unwrap();
        self.selected_gthread.lock().unwrap().replace(gtid);
    }

    #[inline]
    pub fn get_curr_gtid(&self) -> Option<u64> {
        self.selected_gthread.lock().unwrap().clone()
    }

    #[inline]
    pub fn set_curr_session(&self, sid: u64) {
        self.curr_session.lock().unwrap().replace(sid);
    }

    #[inline]
    pub fn get_curr_session(&self) -> Option<u64> {
        self.curr_session.lock().unwrap().clone()
    }

    #[inline]
    pub fn get_gtid(&self, sid: u64, tid: u64) -> Option<u64> {
        let ltid = LocalThreadId::new(sid, tid);
        self.thread_states.get_gtid(&ltid)
    }

    #[inline]
    pub fn remove_thread(&self, sid: u64, tid: u64) -> Option<u64> {
        let ltid = LocalThreadId::new(sid, tid);
        self.thread_states.remove_by_ltid(&ltid)
    }

    #[inline]
    pub fn get_gtgid(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.thread_states
            .get_gtgid(&LocalThreadGroupId::new(sid, tgid))
    }

    #[inline]
    pub fn get_ltid_by_gtid(&self, gtid: u64) -> Option<LocalThreadId> {
        self.thread_states.get_ltid(gtid)
    }

    /// Get all session meta data
    /// This function get copies of all session meta data
    #[inline]
    pub fn get_all_sessions(&self) -> Vec<SessionMetaRef> {
        self.session_states.get_all_sessions()
    }

    /// Get session meta data
    /// This function get a shallow copy of session meta data
    #[inline]
    pub fn get_session(&self, sid: u64) -> Option<SessionMetaRef> {
        self.session_states.get_session(sid)
    }
    
    /// Get session service meta
    /// This function get a deep copy of session service meta data
    #[inline] 
    pub async fn get_session_service_meta(&self, sid: u64) -> Option<ServiceMeta> {
        if let Some(s_meta) = self.get_session(sid) {
            let s_meta = s_meta.read().await;
            return s_meta.service_meta.clone();
        } else {
            return None;
        }
    }

    /// Perform a transaction-like operation on a session meta data
    /// The session meta is locked during the operation (via DashMap internal mechanism)
    /// The function `f` is called with a mutable reference to the session meta data
    // pub async fn with_session_mut<U, F>(&self, sid: u64, f: F) -> Option<U>
    // where
    //     // F: FnOnce(&mut session_mgr::SessionMeta) -> U,
    //     F: FnOnce(tokio::sync::RwLockWriteGuard<'_, SessionMeta>) -> U,
    // {
    //     self.session_states.with_session_mut(sid, f).await
    // }

    /// Perform a transaction-like operation on a session meta data
    /// The session meta is locked during the operation (via DashMap internal mechanism)
    /// The function `f` is called with a immutable reference to the session meta data
    // pub async fn with_session<U, F>(&self, sid: u64, f: F) -> Option<U>
    // where
    //     F: FnOnce(&session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session(sid, f).await
    // }

    // Get session meta data by tag
    // This function get a copy of session meta data
    #[inline]
    pub async fn get_session_by_tag(&self, tag: &str) -> Option<SessionMetaRef> {
        self.session_states.get_session_by_tag(tag).await
    }

    // pub async fn with_session_by_tag<U, F>(&self, tag: &str, f: F) -> Option<U>
    // where
    //     F: FnOnce(&session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session_by_tag(tag, f).await
    // }

    // pub async fn with_session_by_tag_ref<U, F>(&self, tag: &str, f: F) -> Option<U>
    // where
    //     F: FnOnce(&mut session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session_by_tag_ref(tag, f).await
    // }

    // pub async fn with_session_if<P, U, F>(&self, predicate: P, f: F) -> Option<U>
    // where
    //     P: FnMut(&session_mgr::SessionMeta) -> bool,
    //     F: FnOnce(&session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session_if(predicate, f).await
    // }

    // pub async fn with_session_if_mut<P, U, F>(&self, predicate: P, f: F) -> Option<U>
    // where
    //     P: FnMut(&session_mgr::SessionMeta) -> bool,
    //     F: FnOnce(&mut session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session_if_mut(predicate, f).await
    // }

    // pub async fn with_session_all_if<P, U, F>(&self, predicate: P, f: F) -> Vec<U>
    // where
    //     P: FnMut(&session_mgr::SessionMeta) -> bool,
    //     F: Fn(&session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session_all_if(predicate, f).await
    // }

    // pub async fn with_session_all_if_mut<P, U, F>(&self, predicate: P, f: F) -> Vec<U>
    // where
    //     P: FnMut(&session_mgr::SessionMeta) -> bool,
    //     F: Fn(&mut session_mgr::SessionMeta) -> U,
    // {
    //     self.session_states.with_session_all_if_mut(predicate, f).await
    // }

    // pub async fn all_sessions_ref_mut(&self) -> RwLockWriteGuard<'_, HashMap<u64, SessionMeta>> {
    //     self.session_states.all_sessions_ref_mut().await
    // }

    // pub async fn all_sessions_ref(&self) -> RwLockReadGuard<'_, HashMap<u64, SessionMeta>> {
    //     self.session_states.all_sessions_ref().await
    // }

    #[inline]
    pub async fn get_tag_with_tid_by_gtid(&self, gtid: u64) -> Option<(String, u64)> {
        let ltid = self.thread_states.get_ltid(gtid)?;
        let sid = ltid.0;
        let tid = ltid.1;
        let tag = self
            .session_states
            .get_session(sid)?
            .read()
            .await
            .tag
            .clone();
        Some((tag, tid))
    }

    #[inline]
    pub async fn get_tag_by_gtid(&self, gtid: u64) -> Option<String> {
        self.get_tag_with_tid_by_gtid(gtid).await.map(|v| v.0)
    }
}
