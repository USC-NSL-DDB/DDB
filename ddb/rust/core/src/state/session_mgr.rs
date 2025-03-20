use papaya::HashMap as ShardMap;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;

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

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum SessionStatus {
    ON,
    OFF
}

impl Into<&str> for SessionStatus {
    fn into(self) -> &'static str {
        match self {
            SessionStatus::ON => "ON",
            SessionStatus::OFF => "OFF",
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct SessionMeta {
    pub tag: String,
    pub sid: u64,
    pub curr_tid: Option<u64>,
    pub t_status: HashMap<u64, ThreadStatus>,
    pub curr_ctx: Option<ThreadContext>,
    pub in_custom_ctx: bool,

    // indicate of the session is connected or not
    pub status: SessionStatus,

    // maps session unique tid to per inferior tid
    // for example, if session 1 has:
    // tg1: { 1, 2, 4 }
    // tg2: { 3 } then,
    // self.tid_to_per_inferior_tid = { 1: 1, 2: 1, 3: 2, 4: 1 }
    tid_to_per_inferior_tid: HashMap<u64, u64>,

    // maps thread_id (int) to its belonging thread_group_id (str)
    t_to_tg: HashMap<u64, String>,

    // maps thread_group_id (str) to its owning (list of) thread_id (int)
    tg_to_t: HashMap<String, HashSet<u64>>,

    // maps thread_group_id (str) to ThreadGroupStatus
    tg_status: HashMap<String, ThreadGroupStatus>,

    // maps thread_group_id (str) to pid that thread group represents
    tg_to_pid: HashMap<String, u64>,
}

impl SessionMeta {
    #[inline]
    pub fn new(sid: u64, tag: String) -> Self {
        Self {
            tag,
            sid,
            curr_tid: None,
            t_status: HashMap::new(),
            curr_ctx: None,
            in_custom_ctx: false,
            status: SessionStatus::OFF,
            tid_to_per_inferior_tid: HashMap::new(),
            t_to_tg: HashMap::new(),
            tg_to_t: HashMap::new(),
            tg_status: HashMap::new(),
            tg_to_pid: HashMap::new(),
        }
    }

    #[inline]
    pub fn create_thread(&mut self, tid: u64, tgid: &str) {
        self.t_status.insert(tid, ThreadStatus::INIT);
        self.t_to_tg.insert(tid, tgid.to_string());

        let num_exist_threads = self
            .tg_to_t
            .entry(tgid.to_string())
            .or_insert(HashSet::new())
            .len();

        self.tid_to_per_inferior_tid
            .insert(tid, (num_exist_threads + 1) as u64);
        self.tg_to_t
            .entry(tgid.to_string())
            .or_insert(HashSet::new())
            .insert(tid);
    }

    #[inline]
    pub fn add_thread_group(&mut self, tgid: &str) {
        self.tg_to_t
            .entry(tgid.to_string())
            .or_insert(HashSet::new());
        self.tg_status
            .insert(tgid.to_string(), ThreadGroupStatus::INIT);
    }

    #[inline]
    pub fn remove_thread_group(&mut self, tgid: &str) -> HashSet<u64> {
        let associated_threads = self.tg_to_t.get(tgid).cloned().unwrap_or_default();

        for t in &associated_threads {
            self.t_to_tg.remove(t);
            self.t_status.remove(t);
            self.tid_to_per_inferior_tid.remove(t);
        }

        self.tg_to_t.remove(tgid);
        self.tg_status.remove(tgid);
        self.tg_to_pid.remove(tgid);

        associated_threads
    }

    #[inline]
    pub fn start_thread_group(&mut self, tgid: &str, pid: u64) {
        self.tg_status
            .insert(tgid.to_string(), ThreadGroupStatus::RUNNING);
        self.tg_to_pid.insert(tgid.to_string(), pid);
    }

    #[inline]
    pub fn exit_thread_group(&mut self, tgid: &str) {
        self.tg_status
            .insert(tgid.to_string(), ThreadGroupStatus::EXITED);

        if let Some(threads) = self.tg_to_t.get(tgid).cloned() {
            for t in threads {
                self.t_to_tg.remove(&t);
                self.t_status.remove(&t);
            }
            if let Some(thread_set) = self.tg_to_t.get_mut(tgid) {
                thread_set.clear();
            }
        }
    }

    #[allow(unused)]
    #[inline]
    pub fn add_thread_to_group(&mut self, tid: u64, tgid: &str) {
        if !self.tg_to_t.contains_key(tgid) {
            self.add_thread_group(tgid);
        }

        self.tg_to_t
            .entry(tgid.to_string())
            .or_insert(HashSet::new())
            .insert(tid);
        self.t_to_tg.insert(tid, tgid.to_string());
    }

    #[allow(unused)]
    #[inline]
    pub fn get_curr_tid(&self) -> Option<u64> {
        self.curr_tid
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
}

pub type SessionMetaRef = Arc<RwLock<SessionMeta>>;

pub struct SessionStateMgr {
    // sessions: DashMap<u64, SessionMeta>,
    // Note: avoid DashMap as we do need some operations that can
    // enter `.await` point while holding the reference to one or
    // more session meta, which cause deadlock in DashMap.
    // sessions: RwLock<HashMap<u64, SessionMeta>>,
    sessions: ShardMap<u64, SessionMetaRef>,
}

impl SessionStateMgr {
    pub fn new() -> Self {
        Self {
            // sessions: DashMap::new(),
            // sessions: RwLock::new(HashMap::new()),
            sessions: ShardMap::new(),
        }
    }

    #[inline]
    pub async fn add_session(&self, sid: u64, tag: &str) {
        // self.sessions.write().await.insert(
        //     sid,
        //     SessionMeta::new(sid, tag.to_string()),
        //     // Arc::new(RwLock::new(SessionMeta::new(sid, tag.to_string()))),
        // );

        let sessions = self.sessions.pin();
        sessions.insert(
            sid,
            Arc::new(RwLock::new(SessionMeta::new(sid, tag.to_string()))),
        );
    }

    #[inline]
    pub async fn update_session_status(&self, sid: u64, status: SessionStatus) {
        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.update_session_status(status);
        }
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
    /// Get session meta data
    /// This function get a copy of session meta data
    pub fn get_session(&self, sid: u64) -> Option<SessionMetaRef> {
        // self.sessions.get(&sid).map(|v| v.clone())
        // self.sessions.read().await.get(&sid).cloned()

        let sessions = self.sessions.pin();
        sessions.get(&sid).cloned()
    }

    #[inline]
    /// Get all session meta data
    /// This function get copies of all session meta data
    pub fn get_all_sessions(&self) -> Vec<SessionMetaRef> {
        // self.sessions.iter().map(|v| v.value().clone()).collect()

        // self.sessions
        //     .read()
        //     .await
        //     .iter()
        //     .map(|v| v.1.clone())
        //     .collect()

        let sessions = self.sessions.pin();
        sessions.iter().map(|v| v.1.clone()).collect()
    }

    /// Perform a transaction-like operation on a session meta data
    /// The session meta is locked during the operation (via DashMap internal mechanism)
    /// The function `f` is called with a mutable reference to the session meta data
    // pub async fn with_session_mut<U, F>(&self, sid: u64, f: F) -> Option<U>
    // where
    //     F: FnOnce(&mut SessionMetaRef) -> U,
    //     // F: FnOnce(tokio::sync::RwLockWriteGuard<'_, SessionMeta>) -> U,
    // {
    //     // self.sessions.get_mut(&sid).map(|mut v| f(v.value_mut()))

    //     // self.sessions.write().await.get_mut(&sid).map(|v| f(v))

    //     let sessions = self.sessions.pin();
    //     sessions.get(&sid).cloned();
    // }

    /// Perform a transaction-like operation on a session meta data
    /// The session meta is locked during the operation (via DashMap internal mechanism)
    /// The function `f` is called with a immutable reference to the session meta data
    // pub async fn with_session<U, F>(&self, sid: u64, f: F) -> Option<U>
    // where
    //     F: FnOnce(&SessionMeta) -> U,
    // {
    //     // loop {
    //     //     match self.sessions.try_get(&sid) {
    //     //         TryResult::Absent => return None,
    //     //         TryResult::Locked => continue,
    //     //         TryResult::Present(v) => {
    //     //             return Some(f(v.value()));
    //     //         }
    //     //     }
    //     // }
    //     // self.sessions.get(&sid).map(|v| f(v.value()))
    //     self.sessions.read().await.get(&sid).map(|v| f(v))
    // }

    #[inline]
    // Get session meta data by tag
    // This function get a copy of session meta data
    pub async fn get_session_by_tag(&self, tag: &str) -> Option<SessionMetaRef> {
        // self.sessions
        //     .iter()
        //     .find(|v| v.value().tag == tag)
        //     .map(|v| v.value().clone())

        // self.sessions
        //     .read()
        //     .await
        //     .iter()
        //     .find(|v| v.1.tag == tag)
        //     .map(|v| v.1.clone())

        let sessions = self.sessions.pin_owned();
        // Caveats:
        // Try to optimize for large session HashMap to avoid linear iteration.
        // However, this can be a problem sometimes, as it creates many
        // unnecessary tasks that can potentially slow down the scheduler...
        let tasks: Vec<_> = sessions
            .iter()
            .map(|(_, v)| {
                let v = v.clone();
                let tag = tag.to_string();
                tokio::spawn(async move {
                    let session = v.read().await;
                    if session.tag == tag {
                        Some(v.clone())
                    } else {
                        None
                    }
                })
            })
            .collect();

        for result in futures::future::join_all(tasks).await {
            if let Ok(Some(session)) = result {
                return Some(session);
            }
        }
        None
    }

    // pub async fn with_session_by_tag<U, F>(&self, tag: &str, f: F) -> Option<U>
    // where
    //     F: FnOnce(&SessionMeta) -> U,
    // {
    //     // self.sessions
    //     //     .iter()
    //     //     .find(|v| v.value().tag == tag)
    //     //     .map(|v| f(v.value()))

    //     // self.sessions
    //     //     .read()
    //     //     .await
    //     //     .iter()
    //     //     .find(|v| v.1.tag == tag)
    //     //     .map(|v| f(v.1))
    // }

    // pub async fn with_session_by_tag_ref<U, F>(&self, tag: &str, f: F) -> Option<U>
    // where
    //     F: FnOnce(&mut SessionMeta) -> U,
    // {
    //     // self.sessions
    //     //     .iter_mut()
    //     //     .find(|v| v.value().tag == tag)
    //     //     .map(|mut v| f(v.value_mut()))
    //     self.sessions
    //         .write()
    //         .await
    //         .iter_mut()
    //         .find(|v| v.1.tag == tag)
    //         .map(|v| f(v.1))
    // }

    // pub async fn with_session_if<P, U, F>(&self, mut predicate: P, f: F) -> Option<U>
    // where
    //     P: FnMut(&SessionMeta) -> bool,
    //     F: FnOnce(&SessionMeta) -> U,
    // {
    //     // self.sessions
    //     //     .iter()
    //     //     .find(|v| predicate(v.value()))
    //     //     .map(|v| f(v.value()))
    //     self.sessions
    //         .read()
    //         .await
    //         .iter()
    //         .find(|v| predicate(v.1))
    //         .map(|v| f(v.1))
    // }

    // pub async fn with_session_if_mut<P, U, F>(&self, mut predicate: P, f: F) -> Option<U>
    // where
    //     P: FnMut(&SessionMeta) -> bool,
    //     F: FnOnce(&mut SessionMeta) -> U,
    // {
    //     // self.sessions
    //     //     .iter_mut()
    //     //     .find(|v| predicate(v.value()))
    //     //     .map(|mut v| f(v.value_mut()))
    //     self.sessions
    //         .write()
    //         .await
    //         .iter_mut()
    //         .find(|v| predicate(v.1))
    //         .map(|v| f(v.1))
    // }

    // pub async fn with_session_all_if<P, U, F>(&self, mut predicate: P, f: F) -> Vec<U>
    // where
    //     P: FnMut(&SessionMeta) -> bool,
    //     F: Fn(&SessionMeta) -> U,
    // {
    //     // self.sessions
    //     //     .iter()
    //     //     .filter(|v| predicate(v.value()))
    //     //     .map(|v| f(v.value()))
    //     //     .collect()
    //     self.sessions
    //         .read()
    //         .await
    //         .iter()
    //         .filter(|v| predicate(v.1))
    //         .map(|v| f(v.1))
    //         .collect()
    // }

    // pub async fn with_session_all_if_mut<P, U, F>(&self, mut predicate: P, f: F) -> Vec<U>
    // where
    //     P: FnMut(&SessionMeta) -> bool,
    //     F: Fn(&mut SessionMeta) -> U,
    // {
    //     self.sessions
    //         .write()
    //         .await
    //         .iter_mut()
    //         .filter(|v| predicate(v.1))
    //         .map(|v| f(v.1))
    //         .collect()
    // }

    #[inline]
    pub async fn remove_session(&self, sid: u64) {
        // self.sessions.write().await.remove(&sid);

        let sessions = self.sessions.pin();
        sessions.remove(&sid);
    }

    #[inline]
    pub async fn add_thread_group(&self, sid: u64, tgid: &str) {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.add_thread_group(tgid));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.add_thread_group(tgid));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.add_thread_group(tgid);
        }
    }

    #[inline]
    pub async fn create_thread(&self, sid: u64, tid: u64, tgid: &str) {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.create_thread(tid, tgid));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.create_thread(tid, tgid));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.create_thread(tid, tgid);
        }
    }

    #[inline]
    pub async fn remove_thread_group(&self, sid: u64, tgid: &str) -> HashSet<u64> {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.remove_thread_group(tgid))
        //     .unwrap_or_default()

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.remove_thread_group(tgid))
        //     .unwrap_or_default()

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.remove_thread_group(tgid)
        } else {
            HashSet::new()
        }
    }

    #[inline]
    pub async fn start_thread_group(&self, sid: u64, tgid: &str, pid: u64) {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.start_thread_group(tgid, pid));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.start_thread_group(tgid, pid));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.start_thread_group(tgid, pid);
        }
    }

    #[inline]
    pub async fn exit_thread_group(&self, sid: u64, tgid: &str) {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.exit_thread_group(tgid));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.exit_thread_group(tgid));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.exit_thread_group(tgid);
        }
    }

    #[inline]
    pub async fn update_t_status(&self, sid: u64, tid: u64, status: ThreadStatus) {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.update_t_status(tid, status));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.update_t_status(tid, status));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.update_t_status(tid, status);
        }
    }

    #[inline]
    pub async fn update_all_status(&self, sid: u64, new_status: ThreadStatus) {
        // self.sessions
        //     .get_mut(&sid)
        //     .map(|mut v| v.update_all_status(new_status));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.update_all_status(new_status));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.update_all_status(new_status);
        }
    }

    #[inline]
    pub async fn set_curr_tid(&self, sid: u64, tid: u64) {
        // self.sessions.get_mut(&sid).map(|mut v| v.set_curr_tid(tid));

        // self.sessions
        //     .write()
        //     .await
        //     .get_mut(&sid)
        //     .map(|v| v.set_curr_tid(tid));

        let sessions = self.sessions.pin_owned();
        if let Some(session) = sessions.get(&sid) {
            session.write().await.set_curr_tid(tid);
        }
    }

    // pub async fn all_sessions_ref_mut(&self) -> RwLockWriteGuard<'_, HashMap<u64, SessionMeta>> {
    //     self.sessions.write().await
    // }

    // pub async fn all_sessions_ref(&self) -> RwLockReadGuard<'_, HashMap<u64, SessionMeta>> {
    //     self.sessions.read().await
    // }
}
