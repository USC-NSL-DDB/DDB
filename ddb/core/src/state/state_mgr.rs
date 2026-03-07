use std::sync::Mutex;

use crate::{common::counter, discovery::discovery_message_producer::ServiceMeta};

use super::{
    session_mgr,
    thread_mgr::{self, LocalThreadGroupId, LocalThreadId},
    SessionMetaRef,
};

#[derive(Default)]
struct SelectionState {
    curr_session: Mutex<Option<u64>>,
    selected_gthread: Mutex<Option<u64>>,
}

impl SelectionState {
    #[inline]
    fn set_curr_session(&self, sid: u64) {
        self.curr_session.lock().unwrap().replace(sid);
    }

    #[inline]
    fn get_curr_session(&self) -> Option<u64> {
        *self.curr_session.lock().unwrap()
    }

    #[inline]
    fn set_curr_gtid(&self, gtid: u64) {
        self.selected_gthread.lock().unwrap().replace(gtid);
    }

    #[inline]
    fn get_curr_gtid(&self) -> Option<u64> {
        *self.selected_gthread.lock().unwrap()
    }
}

pub struct StateMgr {
    session_states: session_mgr::SessionStateMgr,
    thread_states: thread_mgr::ThreadStateMgr,
    selection: SelectionState,
}

#[allow(unused)]
impl StateMgr {
    pub fn new() -> Self {
        Self {
            session_states: session_mgr::SessionStateMgr::new(),
            thread_states: thread_mgr::ThreadStateMgr::new(),
            selection: SelectionState::default(),
        }
    }

    #[inline]
    fn local_thread_id(sid: u64, tid: u64) -> LocalThreadId {
        LocalThreadId::new(sid, tid)
    }

    #[inline]
    fn local_thread_group_id(sid: u64, tgid: &str) -> LocalThreadGroupId {
        LocalThreadGroupId::new(sid, tgid)
    }

    #[inline]
    fn register_thread_group_index(&self, sid: u64, tgid: &str) -> u64 {
        let gtgid = counter::next_g_inferior_id();
        self.thread_states
            .insert_thread_group(&Self::local_thread_group_id(sid, tgid), gtgid);
        gtgid
    }

    #[inline]
    fn register_thread_index(&self, sid: u64, tid: u64) -> u64 {
        let gtid = counter::next_g_thread_id();
        self.thread_states
            .insert_thread(&Self::local_thread_id(sid, tid), gtid);
        gtid
    }

    #[inline]
    fn remove_thread_indexes<I>(&self, sid: u64, tids: I)
    where
        I: IntoIterator<Item = u64>,
    {
        for tid in tids {
            self.thread_states
                .remove_thread(&Self::local_thread_id(sid, tid));
        }
    }

    #[inline]
    pub async fn register_session(&self, sid: u64, tag: &str, service_meta: Option<ServiceMeta>) {
        self.session_states
            .add_session(sid, tag, service_meta)
            .await;
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
        self.thread_states.global_thread_ids_for_session(sid)
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
        let gtgid = self.register_thread_group_index(sid, tgid);
        self.session_states.add_thread_group(sid, tgid).await;
        gtgid
    }

    #[inline]
    pub async fn remove_thread_group(&self, sid: u64, tgid: &str) -> Option<u64> {
        let local_group_id = Self::local_thread_group_id(sid, tgid);
        let gtgid = self.thread_states.global_thread_group_id(&local_group_id);

        let tids = self.session_states.remove_thread_group(sid, tgid).await;
        self.remove_thread_indexes(sid, tids);

        self.thread_states.remove_thread_group(&local_group_id);
        gtgid
    }

    #[inline]
    pub async fn start_thread_group(&self, sid: u64, tgid: &str, pid: u64) -> Option<u64> {
        self.session_states.start_thread_group(sid, tgid, pid).await;
        self.thread_states
            .global_thread_group_id(&Self::local_thread_group_id(sid, tgid))
    }

    #[inline]
    pub async fn exit_thread_group(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.session_states.exit_thread_group(sid, tgid).await;
        self.thread_states
            .global_thread_group_id(&Self::local_thread_group_id(sid, tgid))
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
        let gtid = self.register_thread_index(sid, tid);
        self.session_states.create_thread(sid, tid, tgid).await;
        let gtgid = self
            .thread_states
            .global_thread_group_id(&Self::local_thread_group_id(sid, tgid))
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

    #[inline]
    pub async fn set_curr_gtid(&self, gtid: u64) {
        let ltid = self.get_ltid_by_gtid(gtid).unwrap();
        self.set_curr_gtid_by_ltid(ltid.0, ltid.1).await;
    }

    #[inline]
    pub async fn set_curr_gtid_by_ltid(&self, sid: u64, tid: u64) {
        self.session_states.set_curr_tid(sid, tid).await;
        let gtid = self.get_gtid(sid, tid).unwrap();
        self.selection.set_curr_gtid(gtid);
    }

    #[inline]
    pub fn get_curr_gtid(&self) -> Option<u64> {
        self.selection.get_curr_gtid()
    }

    #[inline]
    pub fn set_curr_session(&self, sid: u64) {
        self.selection.set_curr_session(sid);
    }

    #[inline]
    pub fn get_curr_session(&self) -> Option<u64> {
        self.selection.get_curr_session()
    }

    #[inline]
    pub fn get_gtid(&self, sid: u64, tid: u64) -> Option<u64> {
        self.thread_states
            .global_thread_id(&Self::local_thread_id(sid, tid))
    }

    #[inline]
    pub fn remove_thread(&self, sid: u64, tid: u64) -> Option<u64> {
        self.thread_states
            .remove_thread(&Self::local_thread_id(sid, tid))
    }

    #[inline]
    pub fn get_gtgid(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.thread_states
            .global_thread_group_id(&Self::local_thread_group_id(sid, tgid))
    }

    #[inline]
    pub fn get_ltid_by_gtid(&self, gtid: u64) -> Option<LocalThreadId> {
        self.thread_states.local_thread_id(gtid)
    }

    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    #[inline]
    pub fn sessions(&self) -> Vec<SessionMetaRef> {
        self.session_states.sessions()
    }

    #[inline]
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    pub fn session(&self, sid: u64) -> Option<SessionMetaRef> {
        self.session_states.session(sid)
    }

    #[inline]
    pub async fn session_service_meta(&self, sid: u64) -> Option<ServiceMeta> {
        self.session_states
            .with_session(sid, |session| session.cloned_service_meta())
            .await
            .flatten()
    }

    #[inline]
    pub fn session_by_tag(&self, tag: &str) -> Option<SessionMetaRef> {
        self.session_states.session_by_tag(tag)
    }

    #[inline]
    pub async fn with_session<U, F>(&self, sid: u64, f: F) -> Option<U>
    where
        F: FnOnce(&session_mgr::SessionMeta) -> U,
    {
        self.session_states.with_session(sid, f).await
    }

    #[inline]
    pub async fn with_session_mut<U, F>(&self, sid: u64, f: F) -> Option<U>
    where
        F: FnOnce(&mut session_mgr::SessionMeta) -> U,
    {
        self.session_states.with_session_mut(sid, f).await
    }

    #[inline]
    pub async fn with_session_by_tag<U, F>(&self, tag: &str, f: F) -> Option<U>
    where
        F: FnOnce(&session_mgr::SessionMeta) -> U,
    {
        self.session_states.with_session_by_tag(tag, f).await
    }

    #[inline]
    pub async fn get_tag_with_tid_by_gtid(&self, gtid: u64) -> Option<(String, u64)> {
        let ltid = self.thread_states.local_thread_id(gtid)?;
        let sid = ltid.0;
        let tid = ltid.1;
        let tag = self
            .with_session(sid, |session| session.tag().to_string())
            .await?;
        Some((tag, tid))
    }

    #[inline]
    pub async fn get_tag_by_gtid(&self, gtid: u64) -> Option<String> {
        self.get_tag_with_tid_by_gtid(gtid).await.map(|v| v.0)
    }

    #[inline]
    pub fn get_all_sessions(&self) -> Vec<SessionMetaRef> {
        self.sessions()
    }

    #[inline]
    pub fn get_session(&self, sid: u64) -> Option<SessionMetaRef> {
        self.session(sid)
    }

    #[inline]
    pub async fn get_session_service_meta(&self, sid: u64) -> Option<ServiceMeta> {
        self.session_service_meta(sid).await
    }

    #[inline]
    pub fn get_session_by_tag(&self, tag: &str) -> Option<SessionMetaRef> {
        self.session_by_tag(tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn state_manager_tracks_threads_groups_and_current_selection() {
        let mgr = StateMgr::new();

        mgr.register_session(1, "svc-a", None).await;
        let gtgid = mgr.add_thread_group(1, "i1").await;
        let (gtid, created_gtgid) = mgr.create_thread(1, 10, "i1").await;

        assert_eq!(created_gtgid, gtgid);
        assert_eq!(mgr.get_gtgid(1, "i1"), Some(gtgid));
        assert_eq!(mgr.get_gtid(1, 10), Some(gtid));
        assert_eq!(mgr.get_ltid_by_gtid(gtid), Some(LocalThreadId::new(1, 10)));

        mgr.set_curr_session(1);
        mgr.set_curr_gtid(gtid).await;

        assert_eq!(mgr.get_curr_session(), Some(1));
        assert_eq!(mgr.get_curr_gtid(), Some(gtid));
        assert_eq!(
            mgr.get_tag_with_tid_by_gtid(gtid).await,
            Some(("svc-a".to_string(), 10))
        );

        assert_eq!(mgr.remove_thread_group(1, "i1").await, Some(gtgid));
        assert!(mgr.get_gtid(1, 10).is_none());
        assert!(mgr.get_gtgid(1, "i1").is_none());
    }
}
