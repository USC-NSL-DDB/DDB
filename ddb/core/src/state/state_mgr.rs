use std::sync::Mutex;

use crate::common::counter::SimpleCounter;

use super::{
    session_mgr,
    thread_mgr::{self, LocalThreadGroupId, LocalThreadId},
    GlobalThreadGroupId, GlobalThreadId, ServiceIdentity, SessionRef,
};

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum StateTransitionError {
    #[error("unknown session {0}")]
    SessionNotFound(u64),
    #[error("unknown thread group {thread_group_id} in session {session_id}")]
    ThreadGroupNotFound {
        session_id: u64,
        thread_group_id: String,
    },
    #[error("unknown thread {thread_id} in session {session_id}")]
    ThreadNotFound { session_id: u64, thread_id: u64 },
    #[error("unknown global thread {0}")]
    GlobalThreadNotFound(GlobalThreadId),
    #[error(
        "thread {thread_id} in session {session_id} belongs to group {actual_group_id}, not {expected_group_id}"
    )]
    ThreadGroupMismatch {
        session_id: u64,
        thread_id: u64,
        expected_group_id: String,
        actual_group_id: String,
    },
}

pub type StateTransitionResult<T> = Result<T, StateTransitionError>;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct GlobalThreadIdentity {
    pub thread_id: GlobalThreadId,
    pub thread_group_id: GlobalThreadGroupId,
}

#[derive(Default)]
struct Selection {
    curr_session: Option<u64>,
    selected_gthread: Option<GlobalThreadId>,
}

#[derive(Default)]
struct SelectionState {
    current: Mutex<Selection>,
}

impl SelectionState {
    #[inline]
    fn select_session(&self, sid: u64) {
        let mut current = self.current.lock().unwrap();
        if current.curr_session != Some(sid) {
            current.selected_gthread = None;
        }
        current.curr_session = Some(sid);
    }

    #[inline]
    fn current_session_id(&self) -> Option<u64> {
        self.current.lock().unwrap().curr_session
    }

    #[inline]
    fn select_thread(&self, sid: u64, gtid: GlobalThreadId) {
        let mut current = self.current.lock().unwrap();
        current.curr_session = Some(sid);
        current.selected_gthread = Some(gtid);
    }

    #[inline]
    fn current_thread_id(&self) -> Option<GlobalThreadId> {
        self.current.lock().unwrap().selected_gthread
    }

    #[inline]
    fn clear_session(&self, sid: u64) {
        let mut current = self.current.lock().unwrap();
        if current.curr_session == Some(sid) {
            current.curr_session = None;
            current.selected_gthread = None;
        }
    }

    #[inline]
    fn clear_thread(&self, gtid: GlobalThreadId) {
        let mut current = self.current.lock().unwrap();
        if current.selected_gthread == Some(gtid) {
            current.selected_gthread = None;
        }
    }
}

pub struct StateMgr {
    session_states: session_mgr::SessionStateMgr,
    thread_states: thread_mgr::ThreadStateMgr,
    selection: SelectionState,
    global_thread_ids: SimpleCounter,
    global_thread_group_ids: SimpleCounter,
}

#[allow(unused)]
impl StateMgr {
    pub fn new() -> Self {
        Self {
            session_states: session_mgr::SessionStateMgr::new(),
            thread_states: thread_mgr::ThreadStateMgr::new(),
            selection: SelectionState::default(),
            global_thread_ids: SimpleCounter::new(),
            global_thread_group_ids: SimpleCounter::new(),
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
    fn register_thread_group_index(&self, sid: u64, tgid: &str) -> GlobalThreadGroupId {
        self.thread_states
            .get_or_insert_thread_group_with(&Self::thread_group_key(sid, tgid), || {
                GlobalThreadGroupId::new(self.global_thread_group_ids.next())
            })
    }

    #[inline]
    fn register_thread_index(&self, sid: u64, tid: u64) -> GlobalThreadId {
        self.thread_states
            .get_or_insert_thread_with(&Self::thread_key(sid, tid), || {
                GlobalThreadId::new(self.global_thread_ids.next())
            })
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
    pub async fn register_session(
        &self,
        sid: u64,
        tag: &str,
        service_identity: Option<ServiceIdentity>,
    ) {
        self.session_states
            .add_session(sid, tag, service_identity)
            .await;
    }

    #[inline]
    pub async fn remove_session(&self, sid: u64) {
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
    pub fn global_thread_ids_for_session(&self, sid: u64) -> Vec<GlobalThreadId> {
        self.thread_states.global_thread_ids_for_session(sid)
    }

    #[inline]
    pub async fn register_thread_group(
        &self,
        sid: u64,
        tgid: &str,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        session.add_thread_group(tgid);
        Ok(self.register_thread_group_index(sid, tgid))
    }

    #[inline]
    pub async fn remove_thread_group(
        &self,
        sid: u64,
        tgid: &str,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let local_group_id = Self::thread_group_key(sid, tgid);
        let gtgid = self
            .thread_states
            .global_thread_group_id(&local_group_id)
            .ok_or_else(|| StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            })?;
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        if !session.contains_thread_group(tgid) {
            return Err(StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            });
        }
        let tids = session.remove_thread_group(tgid);
        self.remove_thread_indexes(sid, tids);
        self.thread_states.remove_thread_group(&local_group_id);
        Ok(gtgid)
    }

    #[inline]
    pub async fn start_thread_group(
        &self,
        sid: u64,
        tgid: &str,
        pid: u64,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let gtgid = self.global_thread_group_id(sid, tgid).ok_or_else(|| {
            StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            }
        })?;
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        if !session.contains_thread_group(tgid) {
            return Err(StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            });
        }
        session.start_thread_group(tgid, pid);
        Ok(gtgid)
    }

    #[inline]
    pub async fn exit_thread_group(
        &self,
        sid: u64,
        tgid: &str,
    ) -> StateTransitionResult<GlobalThreadGroupId> {
        let gtgid = self.global_thread_group_id(sid, tgid).ok_or_else(|| {
            StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            }
        })?;
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        if !session.contains_thread_group(tgid) {
            return Err(StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            });
        }
        let tids = session.exit_thread_group(tgid);
        self.remove_thread_indexes(sid, tids);
        Ok(gtgid)
    }

    #[inline]
    pub async fn register_thread(
        &self,
        sid: u64,
        tid: u64,
        tgid: &str,
    ) -> StateTransitionResult<GlobalThreadIdentity> {
        let gtgid = self.global_thread_group_id(sid, tgid).ok_or_else(|| {
            StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            }
        })?;
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        if !session.contains_thread_group(tgid) {
            return Err(StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: tgid.to_string(),
            });
        }
        if let Some(actual_group_id) = session.thread_group_for(tid) {
            if actual_group_id != tgid {
                return Err(StateTransitionError::ThreadGroupMismatch {
                    session_id: sid,
                    thread_id: tid,
                    expected_group_id: tgid.to_string(),
                    actual_group_id: actual_group_id.to_string(),
                });
            }
        } else {
            session.create_thread(tid, tgid);
        }
        let gtid = self.register_thread_index(sid, tid);
        Ok(GlobalThreadIdentity {
            thread_id: gtid,
            thread_group_id: gtgid,
        })
    }

    #[inline]
    pub async fn remove_thread(
        &self,
        sid: u64,
        tid: u64,
        expected_tgid: &str,
    ) -> StateTransitionResult<GlobalThreadIdentity> {
        let gtid = self
            .global_thread_id(sid, tid)
            .ok_or(StateTransitionError::ThreadNotFound {
                session_id: sid,
                thread_id: tid,
            })?;
        let gtgid = self
            .global_thread_group_id(sid, expected_tgid)
            .ok_or_else(|| StateTransitionError::ThreadGroupNotFound {
                session_id: sid,
                thread_group_id: expected_tgid.to_string(),
            })?;
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        let actual_group_id =
            session
                .thread_group_for(tid)
                .ok_or(StateTransitionError::ThreadNotFound {
                    session_id: sid,
                    thread_id: tid,
                })?;
        if actual_group_id != expected_tgid {
            return Err(StateTransitionError::ThreadGroupMismatch {
                session_id: sid,
                thread_id: tid,
                expected_group_id: expected_tgid.to_string(),
                actual_group_id: actual_group_id.to_string(),
            });
        }
        session.remove_thread(tid);
        self.thread_states
            .remove_thread(&Self::thread_key(sid, tid));
        self.selection.clear_thread(gtid);
        Ok(GlobalThreadIdentity {
            thread_id: gtid,
            thread_group_id: gtgid,
        })
    }

    #[inline]
    pub async fn update_thread_statuses(
        &self,
        sid: u64,
        tids: &[u64],
        status: session_mgr::ThreadStatus,
    ) -> StateTransitionResult<()> {
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        if let Some(tid) = tids
            .iter()
            .find(|tid| session.thread_group_for(**tid).is_none())
        {
            return Err(StateTransitionError::ThreadNotFound {
                session_id: sid,
                thread_id: *tid,
            });
        }
        debug_assert!(session.update_thread_statuses(tids, status));
        Ok(())
    }

    #[inline]
    pub async fn update_all_thread_status(
        &self,
        sid: u64,
        status: session_mgr::ThreadStatus,
    ) -> StateTransitionResult<()> {
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        session.write().await.update_all_status(status);
        Ok(())
    }

    #[inline]
    pub async fn select_thread(&self, gtid: GlobalThreadId) -> StateTransitionResult<()> {
        let local_tid = self
            .local_thread_id(gtid)
            .ok_or(StateTransitionError::GlobalThreadNotFound(gtid))?;
        self.select_local_thread(local_tid.session_id(), local_tid.thread_id())
            .await
    }

    #[inline]
    pub async fn select_local_thread(&self, sid: u64, tid: u64) -> StateTransitionResult<()> {
        let gtid = self
            .global_thread_id(sid, tid)
            .ok_or(StateTransitionError::ThreadNotFound {
                session_id: sid,
                thread_id: tid,
            })?;
        let session = self
            .session_states
            .session(sid)
            .ok_or(StateTransitionError::SessionNotFound(sid))?;
        let mut session = session.write().await;
        if session.thread_group_for(tid).is_none() {
            return Err(StateTransitionError::ThreadNotFound {
                session_id: sid,
                thread_id: tid,
            });
        }
        session.set_curr_tid(tid);
        self.selection.select_thread(sid, gtid);
        Ok(())
    }

    #[inline]
    pub fn select_thread_context(&self, sid: u64, gtid: GlobalThreadId) {
        self.selection.select_thread(sid, gtid);
    }

    #[inline]
    pub fn current_thread_id(&self) -> Option<GlobalThreadId> {
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
    pub(crate) fn read_thread_ids(&self) -> thread_mgr::ThreadIdView<'_> {
        self.thread_states.read_ids()
    }

    #[inline]
    pub fn global_thread_id(&self, sid: u64, tid: u64) -> Option<GlobalThreadId> {
        self.thread_states
            .global_thread_id(&Self::thread_key(sid, tid))
    }

    #[inline]
    pub fn global_thread_group_id(&self, sid: u64, tgid: &str) -> Option<GlobalThreadGroupId> {
        self.thread_states
            .global_thread_group_id(&Self::thread_group_key(sid, tgid))
    }

    #[inline]
    pub fn local_thread_id(&self, gtid: GlobalThreadId) -> Option<LocalThreadId> {
        self.thread_states.local_thread_id(gtid)
    }

    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    #[inline]
    pub fn sessions(&self) -> Vec<SessionRef> {
        self.session_states.sessions()
    }

    #[inline]
    pub fn session_ids(&self) -> Vec<u64> {
        self.session_states.session_ids()
    }

    #[inline]
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    pub fn session(&self, sid: u64) -> Option<SessionRef> {
        self.session_states.session(sid)
    }

    #[inline]
    pub async fn session_service_identity(&self, sid: u64) -> Option<ServiceIdentity> {
        self.session_states
            .with_session(sid, |session| session.cloned_service_identity())
            .await
            .flatten()
    }

    #[inline]
    pub fn session_by_tag(&self, tag: &str) -> Option<SessionRef> {
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
    pub async fn session_tag_and_thread_id(&self, gtid: GlobalThreadId) -> Option<(String, u64)> {
        let local_tid = self.thread_states.local_thread_id(gtid)?;
        let sid = local_tid.session_id();
        let tid = local_tid.thread_id();
        let tag = self
            .with_session(sid, |session| session.tag().to_string())
            .await?;
        Some((tag, tid))
    }

    #[inline]
    pub async fn session_tag_for_thread(&self, gtid: GlobalThreadId) -> Option<String> {
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
        let gtgid = mgr.register_thread_group(1, "i1").await.unwrap();
        let identity = mgr.register_thread(1, 10, "i1").await.unwrap();

        assert_eq!(identity.thread_group_id, gtgid);
        assert_eq!(mgr.global_thread_group_id(1, "i1"), Some(gtgid));
        assert_eq!(mgr.global_thread_id(1, 10), Some(identity.thread_id));
        assert_eq!(
            mgr.local_thread_id(identity.thread_id),
            Some(LocalThreadId::new(1, 10))
        );

        mgr.select_session(1);
        mgr.select_thread(identity.thread_id).await.unwrap();

        assert_eq!(mgr.current_session_id(), Some(1));
        assert_eq!(mgr.current_thread_id(), Some(identity.thread_id));
        assert_eq!(
            mgr.session_tag_and_thread_id(identity.thread_id).await,
            Some(("svc-a".to_string(), 10))
        );

        assert_eq!(mgr.remove_thread_group(1, "i1").await.unwrap(), gtgid);
        assert!(mgr.global_thread_id(1, 10).is_none());
        assert!(mgr.global_thread_group_id(1, "i1").is_none());
    }

    #[tokio::test]
    async fn removing_session_clears_selection_and_thread_indexes() {
        let mgr = StateMgr::new();

        mgr.register_session(1, "svc-a", None).await;
        let gtgid = mgr.register_thread_group(1, "i1").await.unwrap();
        let gtid = mgr.register_thread(1, 10, "i1").await.unwrap().thread_id;

        mgr.select_session(1);
        mgr.select_thread(gtid).await.unwrap();
        mgr.remove_session(1).await;

        assert!(mgr.session(1).is_none());
        assert!(mgr.global_thread_id(1, 10).is_none());
        assert!(mgr.local_thread_id(gtid).is_none());
        assert!(mgr.global_thread_group_id(1, "i1").is_none());
        assert_eq!(mgr.current_session_id(), None);
        assert_eq!(mgr.current_thread_id(), None);
        assert_eq!(mgr.session_tag_and_thread_id(gtid).await, None);
        assert!(gtgid.value() > 0);
    }

    #[tokio::test]
    async fn selection_switches_session_and_thread_as_one_state() {
        let mgr = StateMgr::new();

        mgr.register_session(1, "svc-a", None).await;
        mgr.register_thread_group(1, "i1").await.unwrap();
        let first_gtid = mgr.register_thread(1, 10, "i1").await.unwrap().thread_id;
        mgr.register_session(2, "svc-b", None).await;
        mgr.register_thread_group(2, "i2").await.unwrap();
        let second_gtid = mgr.register_thread(2, 20, "i2").await.unwrap().thread_id;

        mgr.select_thread(first_gtid).await.unwrap();
        assert_eq!(mgr.current_session_id(), Some(1));
        assert_eq!(mgr.current_thread_id(), Some(first_gtid));

        mgr.select_session(2);
        assert_eq!(mgr.current_session_id(), Some(2));
        assert_eq!(mgr.current_thread_id(), None);

        mgr.select_thread(second_gtid).await.unwrap();
        assert_eq!(mgr.current_session_id(), Some(2));
        assert_eq!(mgr.current_thread_id(), Some(second_gtid));
    }

    #[tokio::test]
    async fn duplicate_topology_notifications_keep_global_ids_stable() {
        let mgr = StateMgr::new();
        mgr.register_session(1, "svc-a", None).await;

        let first_group = mgr.register_thread_group(1, "i1").await.unwrap();
        let second_group = mgr.register_thread_group(1, "i1").await.unwrap();
        let first_thread = mgr.register_thread(1, 10, "i1").await.unwrap();
        let second_thread = mgr.register_thread(1, 10, "i1").await.unwrap();

        assert_eq!(first_group, second_group);
        assert_eq!(first_thread, second_thread);
    }

    #[tokio::test]
    async fn removing_thread_cleans_session_metadata_and_selection() {
        let mgr = StateMgr::new();
        mgr.register_session(1, "svc-a", None).await;
        mgr.register_thread_group(1, "i1").await.unwrap();
        let identity = mgr.register_thread(1, 10, "i1").await.unwrap();
        mgr.select_thread(identity.thread_id).await.unwrap();

        let removed = mgr.remove_thread(1, 10, "i1").await.unwrap();

        assert_eq!(removed, identity);
        assert_eq!(mgr.global_thread_id(1, 10), None);
        assert_eq!(mgr.local_thread_id(identity.thread_id), None);
        assert_eq!(mgr.current_thread_id(), None);
        assert_eq!(
            mgr.with_session(1, |session| session
                .thread_group_for(10)
                .map(str::to_string))
                .await,
            Some(None)
        );
        assert_eq!(
            mgr.update_thread_statuses(1, &[10], session_mgr::ThreadStatus::RUNNING)
                .await,
            Err(StateTransitionError::ThreadNotFound {
                session_id: 1,
                thread_id: 10,
            })
        );
    }

    #[tokio::test]
    async fn exiting_group_cleans_every_global_thread_index() {
        let mgr = StateMgr::new();
        mgr.register_session(1, "svc-a", None).await;
        let gtgid = mgr.register_thread_group(1, "i1").await.unwrap();
        let first = mgr.register_thread(1, 10, "i1").await.unwrap();
        let second = mgr.register_thread(1, 11, "i1").await.unwrap();

        assert_eq!(mgr.exit_thread_group(1, "i1").await.unwrap(), gtgid);
        assert_eq!(mgr.global_thread_id(1, 10), None);
        assert_eq!(mgr.global_thread_id(1, 11), None);
        assert_eq!(mgr.local_thread_id(first.thread_id), None);
        assert_eq!(mgr.local_thread_id(second.thread_id), None);
        assert_eq!(mgr.global_thread_group_id(1, "i1"), Some(gtgid));
    }

    #[tokio::test]
    async fn registering_thread_rejects_cross_group_reassignment() {
        let mgr = StateMgr::new();
        mgr.register_session(1, "svc-a", None).await;
        mgr.register_thread_group(1, "i1").await.unwrap();
        mgr.register_thread_group(1, "i2").await.unwrap();
        mgr.register_thread(1, 10, "i1").await.unwrap();

        assert_eq!(
            mgr.register_thread(1, 10, "i2").await,
            Err(StateTransitionError::ThreadGroupMismatch {
                session_id: 1,
                thread_id: 10,
                expected_group_id: "i2".to_string(),
                actual_group_id: "i1".to_string(),
            })
        );
    }
}
