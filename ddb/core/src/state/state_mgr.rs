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
    fn select_session(&self, sid: u64) {
        self.curr_session.lock().unwrap().replace(sid);
    }

    #[inline]
    fn current_session_id(&self) -> Option<u64> {
        *self.curr_session.lock().unwrap()
    }

    #[inline]
    fn select_thread(&self, gtid: u64) {
        self.selected_gthread.lock().unwrap().replace(gtid);
    }

    #[inline]
    fn current_thread_id(&self) -> Option<u64> {
        *self.selected_gthread.lock().unwrap()
    }

    #[inline]
    fn clear_session(&self, sid: u64) {
        let mut current = self.curr_session.lock().unwrap();
        if *current == Some(sid) {
            current.take();
        }
    }

    #[inline]
    fn clear_thread(&self, gtid: u64) {
        let mut current = self.selected_gthread.lock().unwrap();
        if *current == Some(gtid) {
            current.take();
        }
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
    fn thread_key(sid: u64, tid: u64) -> LocalThreadId {
        LocalThreadId::new(sid, tid)
    }

    #[inline]
    fn thread_group_key(sid: u64, tgid: &str) -> LocalThreadGroupId {
        LocalThreadGroupId::new(sid, tgid)
    }

    #[inline]
    fn register_thread_group_index(&self, sid: u64, tgid: &str) -> u64 {
        let gtgid = counter::next_g_inferior_id();
        self.thread_states
            .insert_thread_group(&Self::thread_group_key(sid, tgid), gtgid);
        gtgid
    }

    #[inline]
    fn register_thread_index(&self, sid: u64, tid: u64) -> u64 {
        let gtid = counter::next_g_thread_id();
        self.thread_states
            .insert_thread(&Self::thread_key(sid, tid), gtid);
        gtid
    }

    #[inline]
    fn remove_thread_indexes<I>(&self, sid: u64, tids: I)
    where
        I: IntoIterator<Item = u64>,
    {
        for tid in tids {
            self.thread_states
                .remove_thread(&Self::thread_key(sid, tid));
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
        if let Some(gtid) = self.selection.current_thread_id() {
            if self
                .thread_states
                .local_thread_id(gtid)
                .is_some_and(|local_tid| local_tid.session_id() == sid)
            {
                self.selection.clear_thread(gtid);
            }
        }
        self.selection.clear_session(sid);
        self.thread_states.remove_session(sid);
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
    pub fn global_thread_ids_for_session(&self, sid: u64) -> Vec<u64> {
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
        let local_group_id = Self::thread_group_key(sid, tgid);
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
            .global_thread_group_id(&Self::thread_group_key(sid, tgid))
    }

    #[inline]
    pub async fn exit_thread_group(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.session_states.exit_thread_group(sid, tgid).await;
        self.thread_states
            .global_thread_group_id(&Self::thread_group_key(sid, tgid))
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
            .global_thread_group_id(&Self::thread_group_key(sid, tgid))
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
    pub async fn select_thread(&self, gtid: u64) {
        let local_tid = self.local_thread_id(gtid).unwrap();
        self.select_local_thread(local_tid.session_id(), local_tid.thread_id())
            .await;
    }

    #[inline]
    pub async fn select_local_thread(&self, sid: u64, tid: u64) {
        self.session_states.set_curr_tid(sid, tid).await;
        let gtid = self.global_thread_id(sid, tid).unwrap();
        self.selection.select_thread(gtid);
    }

    #[inline]
    pub fn current_thread_id(&self) -> Option<u64> {
        self.selection.current_thread_id()
    }

    #[inline]
    pub fn select_session(&self, sid: u64) {
        self.selection.select_session(sid);
    }

    #[inline]
    pub fn current_session_id(&self) -> Option<u64> {
        self.selection.current_session_id()
    }

    #[inline]
    pub fn global_thread_id(&self, sid: u64, tid: u64) -> Option<u64> {
        self.thread_states
            .global_thread_id(&Self::thread_key(sid, tid))
    }

    #[inline]
    pub fn remove_thread(&self, sid: u64, tid: u64) -> Option<u64> {
        self.thread_states
            .remove_thread(&Self::thread_key(sid, tid))
    }

    #[inline]
    pub fn global_thread_group_id(&self, sid: u64, tgid: &str) -> Option<u64> {
        self.thread_states
            .global_thread_group_id(&Self::thread_group_key(sid, tgid))
    }

    #[inline]
    pub fn local_thread_id(&self, gtid: u64) -> Option<LocalThreadId> {
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
    pub async fn session_tag_and_thread_id(&self, gtid: u64) -> Option<(String, u64)> {
        let local_tid = self.thread_states.local_thread_id(gtid)?;
        let sid = local_tid.session_id();
        let tid = local_tid.thread_id();
        let tag = self
            .with_session(sid, |session| session.tag().to_string())
            .await?;
        Some((tag, tid))
    }

    #[inline]
    pub async fn session_tag_for_thread(&self, gtid: u64) -> Option<String> {
        self.session_tag_and_thread_id(gtid)
            .await
            .map(|value| value.0)
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
        assert_eq!(mgr.global_thread_group_id(1, "i1"), Some(gtgid));
        assert_eq!(mgr.global_thread_id(1, 10), Some(gtid));
        assert_eq!(mgr.local_thread_id(gtid), Some(LocalThreadId::new(1, 10)));

        mgr.select_session(1);
        mgr.select_thread(gtid).await;

        assert_eq!(mgr.current_session_id(), Some(1));
        assert_eq!(mgr.current_thread_id(), Some(gtid));
        assert_eq!(
            mgr.session_tag_and_thread_id(gtid).await,
            Some(("svc-a".to_string(), 10))
        );

        assert_eq!(mgr.remove_thread_group(1, "i1").await, Some(gtgid));
        assert!(mgr.global_thread_id(1, 10).is_none());
        assert!(mgr.global_thread_group_id(1, "i1").is_none());
    }

    #[tokio::test]
    async fn removing_session_clears_selection_and_thread_indexes() {
        let mgr = StateMgr::new();

        mgr.register_session(1, "svc-a", None).await;
        let gtgid = mgr.add_thread_group(1, "i1").await;
        let (gtid, _) = mgr.create_thread(1, 10, "i1").await;

        mgr.select_session(1);
        mgr.select_thread(gtid).await;

        mgr.remove_session(1).await;

        assert!(mgr.session(1).is_none());
        assert!(mgr.global_thread_id(1, 10).is_none());
        assert!(mgr.local_thread_id(gtid).is_none());
        assert!(mgr.global_thread_group_id(1, "i1").is_none());
        assert_eq!(mgr.current_session_id(), None);
        assert_eq!(mgr.current_thread_id(), None);
        assert_eq!(mgr.session_tag_and_thread_id(gtid).await, None);
        assert!(gtgid > 0);
    }
}
