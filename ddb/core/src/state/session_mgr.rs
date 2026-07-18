use dashmap::DashMap;
use papaya::HashMap as ShardMap;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};
use tokio::sync::{
    Mutex as TokioMutex, OwnedMutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
};

use crate::discovery::discovery_message_producer::ServiceMeta;

#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub enum ThreadStatus {
    INIT,
    STOPPED,
    RUNNING,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub enum ThreadGroupStatus {
    INIT,
    STOPPED,
    RUNNING,
    EXITED,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ThreadContext {
    pub tid: u64,
    pub ctx: HashMap<String, u64>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum SessionStatus {
    ON,
    OFF,
}

impl SessionStatus {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::ON => "ON",
            SessionStatus::OFF => "OFF",
        }
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
struct ThreadGroupMeta {
    threads: HashSet<u64>,
    status: ThreadGroupStatus,
    pid: Option<u64>,
}

impl Default for ThreadGroupMeta {
    fn default() -> Self {
        Self {
            threads: HashSet::new(),
            status: ThreadGroupStatus::INIT,
            pid: None,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Default)]
struct SessionThreadRegistry {
    tid_to_per_inferior_tid: HashMap<u64, u64>,
    tid_to_group: HashMap<u64, String>,
    groups: HashMap<String, ThreadGroupMeta>,
}

impl SessionThreadRegistry {
    #[inline]
    fn ensure_group(&mut self, tgid: &str) -> &mut ThreadGroupMeta {
        self.groups.entry(tgid.to_string()).or_default()
    }

    #[inline]
    fn add_thread_group(&mut self, tgid: &str) {
        self.ensure_group(tgid);
    }

    #[inline]
    fn create_thread(
        &mut self,
        tid: u64,
        tgid: &str,
        thread_statuses: &mut HashMap<u64, ThreadStatus>,
    ) {
        thread_statuses.insert(tid, ThreadStatus::INIT);

        let per_inferior_tid = {
            let group = self.ensure_group(tgid);
            let per_inferior_tid = (group.threads.len() + 1) as u64;
            group.threads.insert(tid);
            per_inferior_tid
        };

        self.tid_to_group.insert(tid, tgid.to_string());
        self.tid_to_per_inferior_tid.insert(tid, per_inferior_tid);
    }

    #[inline]
    fn remove_thread_group(
        &mut self,
        tgid: &str,
        thread_statuses: &mut HashMap<u64, ThreadStatus>,
    ) -> HashSet<u64> {
        let Some(group) = self.groups.remove(tgid) else {
            return HashSet::new();
        };

        for tid in &group.threads {
            self.remove_thread_metadata(*tid, thread_statuses);
        }

        group.threads
    }

    #[inline]
    fn start_thread_group(&mut self, tgid: &str, pid: u64) {
        let group = self.ensure_group(tgid);
        group.status = ThreadGroupStatus::RUNNING;
        group.pid = Some(pid);
    }

    #[inline]
    fn exit_thread_group(&mut self, tgid: &str, thread_statuses: &mut HashMap<u64, ThreadStatus>) {
        let group = self.ensure_group(tgid);
        group.status = ThreadGroupStatus::EXITED;

        let threads = std::mem::take(&mut group.threads);
        for tid in threads {
            self.remove_thread_metadata(tid, thread_statuses);
        }
    }

    #[allow(unused)]
    #[inline]
    fn add_thread_to_group(&mut self, tid: u64, tgid: &str) {
        self.ensure_group(tgid).threads.insert(tid);
        self.tid_to_group.insert(tid, tgid.to_string());
    }

    #[inline]
    fn remove_thread_metadata(
        &mut self,
        tid: u64,
        thread_statuses: &mut HashMap<u64, ThreadStatus>,
    ) {
        self.tid_to_group.remove(&tid);
        self.tid_to_per_inferior_tid.remove(&tid);
        thread_statuses.remove(&tid);
    }

    #[cfg(test)]
    #[inline]
    fn per_inferior_tid(&self, tid: u64) -> Option<u64> {
        self.tid_to_per_inferior_tid.get(&tid).copied()
    }

    #[cfg(test)]
    #[inline]
    fn thread_group_for(&self, tid: u64) -> Option<&str> {
        self.tid_to_group.get(&tid).map(String::as_str)
    }

    #[cfg(test)]
    #[inline]
    fn group_threads_len(&self, tgid: &str) -> Option<usize> {
        self.groups.get(tgid).map(|group| group.threads.len())
    }

    #[cfg(test)]
    #[inline]
    fn group_status(&self, tgid: &str) -> Option<ThreadGroupStatus> {
        self.groups.get(tgid).map(|group| group.status)
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct SessionMeta {
    tag: String,
    sid: u64,
    curr_tid: Option<u64>,
    t_status: HashMap<u64, ThreadStatus>,
    curr_ctx: Option<ThreadContext>,
    in_custom_ctx: bool,

    service_meta: Option<ServiceMeta>,

    // indicate of the session is connected or not
    status: SessionStatus,

    // Stores the per-session thread/group topology and keeps the related
    // indexes updated together instead of spreading that bookkeeping across
    // multiple HashMaps in SessionMeta itself.
    threads: SessionThreadRegistry,
}

impl SessionMeta {
    #[inline]
    pub fn new(sid: u64, tag: String, service_meta: Option<ServiceMeta>) -> Self {
        Self {
            tag,
            sid,
            curr_tid: None,
            t_status: HashMap::new(),
            curr_ctx: None,
            in_custom_ctx: false,
            service_meta,
            status: SessionStatus::OFF,
            threads: SessionThreadRegistry::default(),
        }
    }

    #[inline]
    pub fn create_thread(&mut self, tid: u64, tgid: &str) {
        self.threads.create_thread(tid, tgid, &mut self.t_status);
    }

    #[inline]
    pub fn add_thread_group(&mut self, tgid: &str) {
        self.threads.add_thread_group(tgid);
    }

    #[inline]
    pub fn remove_thread_group(&mut self, tgid: &str) -> HashSet<u64> {
        self.threads.remove_thread_group(tgid, &mut self.t_status)
    }

    #[inline]
    pub fn start_thread_group(&mut self, tgid: &str, pid: u64) {
        self.threads.start_thread_group(tgid, pid);
    }

    #[inline]
    pub fn exit_thread_group(&mut self, tgid: &str) {
        self.threads.exit_thread_group(tgid, &mut self.t_status);
    }

    #[allow(unused)]
    #[inline]
    pub fn add_thread_to_group(&mut self, tid: u64, tgid: &str) {
        self.threads.add_thread_to_group(tid, tgid);
    }

    #[allow(unused)]
    #[inline]
    pub fn get_curr_tid(&self) -> Option<u64> {
        self.curr_tid
    }

    #[inline]
    pub fn sid(&self) -> u64 {
        self.sid
    }

    #[inline]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    #[inline]
    pub fn service_meta(&self) -> Option<&ServiceMeta> {
        self.service_meta.as_ref()
    }

    #[inline]
    pub fn cloned_service_meta(&self) -> Option<ServiceMeta> {
        self.service_meta.clone()
    }

    #[inline]
    pub fn status(&self) -> SessionStatus {
        self.status
    }

    #[inline]
    pub fn current_context(&self) -> Option<&ThreadContext> {
        self.curr_ctx.as_ref()
    }

    #[inline]
    pub fn set_current_context(&mut self, ctx: Option<ThreadContext>) {
        self.curr_ctx = ctx;
    }

    #[inline]
    pub fn is_in_custom_context(&self) -> bool {
        self.in_custom_ctx
    }

    #[inline]
    pub fn set_in_custom_context(&mut self, in_custom_ctx: bool) {
        self.in_custom_ctx = in_custom_ctx;
    }

    #[inline]
    pub fn all_threads_stopped(&self) -> bool {
        self.t_status
            .values()
            .all(|status| *status == ThreadStatus::STOPPED)
    }

    #[inline]
    pub fn set_curr_tid(&mut self, tid: u64) {
        self.curr_tid = Some(tid);
    }

    #[inline]
    pub fn update_t_status(&mut self, tid: u64, status: ThreadStatus) {
        self.t_status.insert(tid, status);
    }

    #[inline]
    pub fn update_all_status(&mut self, new_status: ThreadStatus) {
        for (_, status) in self.t_status.iter_mut() {
            *status = new_status;
        }
    }

    #[inline]
    pub fn update_session_status(&mut self, status: SessionStatus) {
        self.status = status;
    }

    #[cfg(test)]
    #[inline]
    fn per_inferior_tid(&self, tid: u64) -> Option<u64> {
        self.threads.per_inferior_tid(tid)
    }

    #[cfg(test)]
    #[inline]
    fn thread_group_for(&self, tid: u64) -> Option<&str> {
        self.threads.thread_group_for(tid)
    }

    #[cfg(test)]
    #[inline]
    fn thread_group_len(&self, tgid: &str) -> Option<usize> {
        self.threads.group_threads_len(tgid)
    }

    #[cfg(test)]
    #[inline]
    fn thread_group_status(&self, tgid: &str) -> Option<ThreadGroupStatus> {
        self.threads.group_status(tgid)
    }
}

pub type SessionReadGuard<'a> = RwLockReadGuard<'a, SessionMeta>;
pub type SessionWriteGuard<'a> = RwLockWriteGuard<'a, SessionMeta>;

/// Wrapper containing session metadata and transaction lock.
///
/// The `meta` field holds the actual session state protected by RwLock.
/// The `tx_lock` provides exclusive access for command transaction sequences.
#[derive(Debug)]
pub struct SessionWrapper {
    /// Session metadata protected by read-write lock
    meta: RwLock<SessionMeta>,
    /// Transaction lock - acquire for exclusive command sequence access.
    /// Uses Arc to support OwnedMutexGuard.
    tx_lock: Arc<TokioMutex<()>>,
}

impl SessionWrapper {
    #[inline]
    pub async fn read(&self) -> SessionReadGuard<'_> {
        self.meta.read().await
    }

    #[inline]
    pub async fn read_with<U, F>(&self, f: F) -> U
    where
        F: FnOnce(&SessionMeta) -> U,
    {
        let session = self.read().await;
        f(&session)
    }

    #[inline]
    pub async fn write(&self) -> SessionWriteGuard<'_> {
        self.meta.write().await
    }

    #[inline]
    pub async fn write_with<U, F>(&self, f: F) -> U
    where
        F: FnOnce(&mut SessionMeta) -> U,
    {
        let mut session = self.write().await;
        f(&mut session)
    }

    #[inline]
    pub async fn lock_transaction_owned(&self) -> OwnedMutexGuard<()> {
        self.tx_lock.clone().lock_owned().await
    }
}

/// Reference to a session wrapper
pub type SessionRef = Arc<SessionWrapper>;

/// Backward compatibility alias
pub type SessionMetaRef = SessionRef;

pub struct SessionStateMgr {
    // Avoid DashMap here because session operations frequently hold a session
    // lock across `.await`, which would make shard-locked references unsafe to use.
    sessions: ShardMap<u64, SessionRef>,
    tag_index: DashMap<String, u64>,
}

impl SessionStateMgr {
    pub fn new() -> Self {
        Self {
            sessions: ShardMap::new(),
            tag_index: DashMap::new(),
        }
    }

    #[inline]
    pub async fn add_session(&self, sid: u64, tag: &str, service_meta: Option<ServiceMeta>) {
        let sessions = self.sessions.pin();
        sessions.insert(
            sid,
            Arc::new(SessionWrapper {
                meta: RwLock::new(SessionMeta::new(sid, tag.to_string(), service_meta)),
                tx_lock: Arc::new(TokioMutex::new(())),
            }),
        );
        self.tag_index.insert(tag.to_string(), sid);
    }

    #[inline]
    pub async fn update_session_status(&self, sid: u64, status: SessionStatus) {
        self.update_session_with(sid, |session| session.update_session_status(status))
            .await;
    }

    #[inline]
    pub async fn update_session_status_on(&self, sid: u64) {
        self.update_session_status(sid, SessionStatus::ON).await;
    }

    #[inline]
    pub async fn update_session_status_off(&self, sid: u64) {
        self.update_session_status(sid, SessionStatus::OFF).await;
    }

    #[inline]
    pub fn session(&self, sid: u64) -> Option<SessionRef> {
        let sessions = self.sessions.pin();
        sessions.get(&sid).cloned()
    }

    #[inline]
    pub fn sessions(&self) -> Vec<SessionRef> {
        let sessions = self.sessions.pin();
        sessions.iter().map(|v| v.1.clone()).collect()
    }

    #[inline]
    pub fn session_by_tag(&self, tag: &str) -> Option<SessionRef> {
        self.tag_index
            .get(tag)
            .and_then(|sid| self.session(*sid.value()))
    }

    #[inline]
    pub async fn remove_session(&self, sid: u64) {
        let tag = if let Some(session) = self.session(sid) {
            Some(session.read_with(|meta| meta.tag().to_string()).await)
        } else {
            None
        };
        let sessions = self.sessions.pin();
        sessions.remove(&sid);
        drop(sessions);
        if let Some(tag) = tag {
            self.tag_index.remove(&tag);
        } else {
            self.tag_index.retain(|_, indexed_sid| *indexed_sid != sid);
        }
    }

    #[inline]
    pub async fn add_thread_group(&self, sid: u64, tgid: &str) {
        self.update_session_with(sid, |session| session.add_thread_group(tgid))
            .await;
    }

    #[inline]
    pub async fn create_thread(&self, sid: u64, tid: u64, tgid: &str) {
        self.update_session_with(sid, |session| session.create_thread(tid, tgid))
            .await;
    }

    #[inline]
    pub async fn remove_thread_group(&self, sid: u64, tgid: &str) -> HashSet<u64> {
        self.with_session_mut(sid, |session| session.remove_thread_group(tgid))
            .await
            .unwrap_or_default()
    }

    #[inline]
    pub async fn start_thread_group(&self, sid: u64, tgid: &str, pid: u64) {
        self.update_session_with(sid, |session| session.start_thread_group(tgid, pid))
            .await;
    }

    #[inline]
    pub async fn exit_thread_group(&self, sid: u64, tgid: &str) {
        self.update_session_with(sid, |session| session.exit_thread_group(tgid))
            .await;
    }

    #[inline]
    pub async fn update_t_status(&self, sid: u64, tid: u64, status: ThreadStatus) {
        self.update_session_with(sid, |session| session.update_t_status(tid, status))
            .await;
    }

    #[inline]
    pub async fn update_all_status(&self, sid: u64, new_status: ThreadStatus) {
        self.update_session_with(sid, |session| session.update_all_status(new_status))
            .await;
    }

    #[inline]
    pub async fn update_session_with<F: FnOnce(&mut SessionMeta)>(&self, sid: u64, f: F) {
        let _ = self.with_session_mut(sid, f).await;
    }

    #[inline]
    pub async fn set_curr_tid(&self, sid: u64, tid: u64) {
        self.update_session_with(sid, |session| session.set_curr_tid(tid))
            .await;
    }

    #[inline]
    pub async fn with_session<U, F>(&self, sid: u64, f: F) -> Option<U>
    where
        F: FnOnce(&SessionMeta) -> U,
    {
        let session = self.session(sid)?;
        Some(session.read_with(f).await)
    }

    #[inline]
    pub async fn with_session_mut<U, F>(&self, sid: u64, f: F) -> Option<U>
    where
        F: FnOnce(&mut SessionMeta) -> U,
    {
        let session = self.session(sid)?;
        Some(session.write_with(f).await)
    }

    #[inline]
    pub async fn with_session_by_tag<U, F>(&self, tag: &str, f: F) -> Option<U>
    where
        F: FnOnce(&SessionMeta) -> U,
    {
        let session = self.session_by_tag(tag)?;
        Some(session.read_with(f).await)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn session_meta_removing_thread_group_cleans_thread_indexes() {
        let mut meta = SessionMeta::new(1, "svc-a".to_string(), None);

        meta.add_thread_group("i1");
        meta.add_thread_group("i2");
        meta.create_thread(10, "i1");
        meta.create_thread(11, "i1");
        meta.create_thread(20, "i2");

        assert_eq!(meta.per_inferior_tid(10), Some(1));
        assert_eq!(meta.per_inferior_tid(11), Some(2));
        assert_eq!(meta.per_inferior_tid(20), Some(1));

        let removed = meta.remove_thread_group("i1");

        assert_eq!(removed, HashSet::from([10, 11]));
        assert!(meta.thread_group_for(10).is_none());
        assert!(!meta.t_status.contains_key(&10));
        assert!(meta.per_inferior_tid(10).is_none());
        assert_eq!(meta.thread_group_len("i2"), Some(1));
    }

    #[test]
    fn session_meta_exiting_thread_group_marks_it_exited_and_clears_threads() {
        let mut meta = SessionMeta::new(1, "svc-a".to_string(), None);

        meta.add_thread_group("i1");
        meta.create_thread(10, "i1");
        meta.create_thread(11, "i1");
        meta.exit_thread_group("i1");

        assert_eq!(
            meta.thread_group_status("i1"),
            Some(ThreadGroupStatus::EXITED)
        );
        assert_eq!(meta.thread_group_len("i1"), Some(0));
        assert!(meta.thread_group_for(10).is_none());
        assert!(!meta.t_status.contains_key(&10));
        assert!(meta.thread_group_for(11).is_none());
        assert!(!meta.t_status.contains_key(&11));
    }

    #[tokio::test]
    async fn session_state_manager_can_find_session_by_tag() {
        let mgr = SessionStateMgr::new();
        mgr.add_session(1, "svc-a", None).await;
        mgr.add_session(2, "svc-b", None).await;

        let session = mgr
            .session_by_tag("svc-b")
            .expect("session should be found by tag");

        assert_eq!(session.read_with(|meta| meta.sid()).await, 2);
    }
}
