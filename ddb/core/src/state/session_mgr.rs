use dashmap::DashMap;
use papaya::HashMap as ShardMap;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::state::{GlobalThreadId, ServiceIdentity};

/// Backend-neutral frame-zero location retained only for the current stopped
/// execution generation. Running a thread clears this value.
#[derive(Debug, Eq, PartialEq, Clone, Default, serde::Serialize)]
pub(crate) struct ThreadLocation {
    pub(crate) path: Option<String>,
    pub(crate) line: Option<u64>,
    pub(crate) column: Option<u64>,
    pub(crate) address: Option<String>,
    pub(crate) function_name: Option<String>,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub(crate) struct LocalThreadStateSnapshot {
    pub(crate) local_id: u64,
    pub(crate) status: ThreadStatus,
    pub(crate) execution_revision: u64,
    pub(crate) location: Option<ThreadLocation>,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
#[allow(clippy::upper_case_acronyms)]
pub enum ThreadStatus {
    INIT,
    STOPPED,
    RUNNING,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
#[allow(clippy::upper_case_acronyms)]
pub enum ThreadGroupStatus {
    INIT,
    RUNNING,
    EXITED,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ThreadContext {
    pub tid: GlobalThreadId,
    pub ctx: HashMap<String, u64>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
#[allow(clippy::upper_case_acronyms)]
pub enum SessionStatus {
    STARTING,
    ON,
    OFF,
}

impl SessionStatus {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::STARTING => "OFF",
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
    fn contains_group(&self, tgid: &str) -> bool {
        self.groups.contains_key(tgid)
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
    ) -> bool {
        if self.tid_to_group.contains_key(&tid) {
            return false;
        }
        thread_statuses.insert(tid, ThreadStatus::INIT);

        let per_inferior_tid = {
            let group = self.ensure_group(tgid);
            let per_inferior_tid = (group.threads.len() + 1) as u64;
            group.threads.insert(tid);
            per_inferior_tid
        };

        self.tid_to_group.insert(tid, tgid.to_string());
        self.tid_to_per_inferior_tid.insert(tid, per_inferior_tid);
        true
    }

    #[inline]
    fn remove_thread(
        &mut self,
        tid: u64,
        thread_statuses: &mut HashMap<u64, ThreadStatus>,
    ) -> Option<String> {
        let tgid = self.tid_to_group.get(&tid)?.clone();
        if let Some(group) = self.groups.get_mut(&tgid) {
            group.threads.remove(&tid);
        }
        self.remove_thread_metadata(tid, thread_statuses);
        Some(tgid)
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
    fn exit_thread_group(
        &mut self,
        tgid: &str,
        thread_statuses: &mut HashMap<u64, ThreadStatus>,
    ) -> HashSet<u64> {
        let Some(group) = self.groups.get_mut(tgid) else {
            return HashSet::new();
        };
        group.status = ThreadGroupStatus::EXITED;

        let threads = std::mem::take(&mut group.threads);
        for tid in &threads {
            self.remove_thread_metadata(*tid, thread_statuses);
        }
        threads
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
    // Monotonic execution generation for each thread. Frontend resources such
    // as frames and variables are valid only while this value is unchanged.
    t_revisions: HashMap<u64, u64>,
    t_locations: HashMap<u64, ThreadLocation>,
    curr_ctx: Option<ThreadContext>,
    in_custom_ctx: bool,

    service_identity: Option<ServiceIdentity>,

    // indicate of the session is connected or not
    status: SessionStatus,

    // Stores the per-session thread/group topology and keeps the related
    // indexes updated together instead of spreading that bookkeeping across
    // multiple HashMaps in SessionMeta itself.
    threads: SessionThreadRegistry,
}

impl SessionMeta {
    #[inline]
    pub fn new(sid: u64, tag: String, service_identity: Option<ServiceIdentity>) -> Self {
        Self {
            tag,
            sid,
            curr_tid: None,
            t_status: HashMap::new(),
            t_revisions: HashMap::new(),
            t_locations: HashMap::new(),
            curr_ctx: None,
            in_custom_ctx: false,
            service_identity,
            status: SessionStatus::STARTING,
            threads: SessionThreadRegistry::default(),
        }
    }

    #[inline]
    pub fn create_thread(&mut self, tid: u64, tgid: &str) -> bool {
        let created = self.threads.create_thread(tid, tgid, &mut self.t_status);
        if created {
            self.t_revisions.insert(tid, 1);
        }
        created
    }

    #[inline]
    pub fn contains_thread_group(&self, tgid: &str) -> bool {
        self.threads.contains_group(tgid)
    }

    #[inline]
    pub fn thread_group_for(&self, tid: u64) -> Option<&str> {
        self.threads.thread_group_for(tid)
    }

    #[inline]
    pub fn remove_thread(&mut self, tid: u64) -> Option<String> {
        let removed = self.threads.remove_thread(tid, &mut self.t_status);
        if removed.is_some() {
            self.t_revisions.remove(&tid);
            self.t_locations.remove(&tid);
        }
        removed
    }

    #[inline]
    pub fn add_thread_group(&mut self, tgid: &str) {
        self.threads.add_thread_group(tgid);
    }

    #[inline]
    pub fn remove_thread_group(&mut self, tgid: &str) -> HashSet<u64> {
        let removed = self.threads.remove_thread_group(tgid, &mut self.t_status);
        for tid in &removed {
            self.t_revisions.remove(tid);
            self.t_locations.remove(tid);
        }
        removed
    }

    #[inline]
    pub fn start_thread_group(&mut self, tgid: &str, pid: u64) {
        self.threads.start_thread_group(tgid, pid);
    }

    #[inline]
    pub fn exit_thread_group(&mut self, tgid: &str) -> HashSet<u64> {
        let removed = self.threads.exit_thread_group(tgid, &mut self.t_status);
        for tid in &removed {
            self.t_revisions.remove(tid);
            self.t_locations.remove(tid);
        }
        removed
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
    pub fn cloned_service_identity(&self) -> Option<ServiceIdentity> {
        self.service_identity.clone()
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
    pub fn is_in_custom_context(&self) -> bool {
        self.in_custom_ctx
    }

    /// Enters a custom thread context: the stored context and the flag change
    /// as one operation, so no caller can flip the flag without a context.
    #[inline]
    pub fn enter_custom_context(&mut self, ctx: ThreadContext) {
        self.curr_ctx = Some(ctx);
        self.in_custom_ctx = true;
    }

    /// Records the outcome of a context-restore attempt: the session leaves
    /// the custom context exactly when the restore succeeded.
    #[inline]
    pub fn exit_custom_context(&mut self, restored: bool) {
        self.in_custom_ctx = !restored;
    }

    #[inline]
    pub fn all_threads_stopped(&self) -> bool {
        self.t_status
            .values()
            .all(|status| *status == ThreadStatus::STOPPED)
    }

    /// Returns detached local thread status for read-only public projections.
    pub(crate) fn thread_status_snapshot(&self) -> (Option<u64>, Vec<LocalThreadStateSnapshot>) {
        let mut threads = self
            .t_status
            .iter()
            .map(|(local_id, status)| LocalThreadStateSnapshot {
                local_id: *local_id,
                status: *status,
                execution_revision: self.t_revisions.get(local_id).copied().unwrap_or(1),
                location: self.t_locations.get(local_id).cloned(),
            })
            .collect::<Vec<_>>();
        threads.sort_unstable_by_key(|thread| thread.local_id);
        (self.curr_tid, threads)
    }

    /// Returns detached inferior identities and operating-system PIDs for
    /// public process projections. Backend-local group IDs remain inside the
    /// runtime boundary and are used only to resolve DDB's global identity.
    pub(crate) fn thread_group_snapshot(&self) -> Vec<(String, Option<u64>)> {
        let mut groups = self
            .threads
            .groups
            .iter()
            .map(|(local_id, group)| (local_id.clone(), group.pid))
            .collect::<Vec<_>>();
        groups.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        groups
    }

    #[inline]
    pub fn set_curr_tid(&mut self, tid: u64) {
        self.curr_tid = Some(tid);
    }

    #[inline]
    pub fn update_thread_statuses(&mut self, tids: &[u64], status: ThreadStatus) -> bool {
        if tids.iter().any(|tid| !self.t_status.contains_key(tid)) {
            return false;
        }
        for tid in tids {
            if status != ThreadStatus::STOPPED {
                self.t_locations.remove(tid);
            }
            let previous = self.t_status.insert(*tid, status);
            if previous != Some(status) {
                let revision = self.t_revisions.entry(*tid).or_insert(1);
                *revision = revision.saturating_add(1);
            }
        }
        true
    }

    pub(crate) fn set_thread_location(&mut self, tid: u64, location: ThreadLocation) -> bool {
        if self.t_status.get(&tid) != Some(&ThreadStatus::STOPPED) {
            return false;
        }
        if self.t_locations.get(&tid) == Some(&location) {
            return true;
        }
        self.t_locations.insert(tid, location);
        let revision = self.t_revisions.entry(tid).or_insert(1);
        *revision = revision.saturating_add(1);
        true
    }

    #[inline]
    pub fn update_all_status(&mut self, new_status: ThreadStatus) {
        if new_status != ThreadStatus::STOPPED {
            self.t_locations.clear();
        }
        for (tid, status) in self.t_status.iter_mut() {
            if *status != new_status {
                *status = new_status;
                let revision = self.t_revisions.entry(*tid).or_insert(1);
                *revision = revision.saturating_add(1);
            }
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

/// Wrapper containing session metadata.
#[derive(Debug)]
pub struct SessionWrapper {
    /// Session metadata protected by read-write lock
    meta: RwLock<SessionMeta>,
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
}

/// Reference to a session wrapper
pub type SessionRef = Arc<SessionWrapper>;

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
    pub async fn add_session(
        &self,
        sid: u64,
        tag: &str,
        service_identity: Option<ServiceIdentity>,
    ) {
        let sessions = self.sessions.pin();
        sessions.insert(
            sid,
            Arc::new(SessionWrapper {
                meta: RwLock::new(SessionMeta::new(sid, tag.to_string(), service_identity)),
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
    pub fn session_ids(&self) -> Vec<u64> {
        let sessions = self.sessions.pin();
        sessions.keys().copied().collect()
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
    pub async fn update_session_with<F: FnOnce(&mut SessionMeta)>(&self, sid: u64, f: F) {
        let _ = self.with_session_mut(sid, f).await;
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

    #[test]
    fn stopped_location_is_revisioned_and_cleared_when_execution_resumes() {
        let mut meta = SessionMeta::new(1, "svc-a".to_string(), None);
        meta.add_thread_group("i1");
        meta.create_thread(10, "i1");
        assert!(meta.update_thread_statuses(&[10], ThreadStatus::STOPPED));
        assert!(meta.set_thread_location(
            10,
            ThreadLocation {
                path: Some("src/main.rs".to_string()),
                line: Some(42),
                ..ThreadLocation::default()
            }
        ));
        let (_, stopped) = meta.thread_status_snapshot();
        let stopped_revision = stopped[0].execution_revision;
        assert_eq!(
            stopped[0]
                .location
                .as_ref()
                .and_then(|location| location.line),
            Some(42)
        );

        assert!(meta.update_thread_statuses(&[10], ThreadStatus::RUNNING));
        let (_, running) = meta.thread_status_snapshot();
        assert!(running[0].execution_revision > stopped_revision);
        assert!(running[0].location.is_none());
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
