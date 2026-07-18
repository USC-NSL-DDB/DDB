use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Result};
use dashmap::DashMap;
use serde::Deserialize;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use super::{
    input::{Command, ParsedInputCmd},
    tracker::COMMAND_TIMEOUT,
    DynFormatter, FinishedCmd, OutputSource, PlainFormatter, Tracker,
};
use crate::{
    cmd_flow::NullFormatter,
    dbg_ctrl::InputSender,
    get_dbg_mgr,
    state::{
        get_bkpt_mgr, get_group_mgr, get_proclet_mgr, get_source_mgr, GroupId, LocalThreadId,
        STATES,
    },
};

const COMMAND_SEND_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Target {
    Session(u64),
    Thread(u64),
    Group(GroupId),
    CurrThread,
    CurrSession,
    SessionSet(HashSet<u64>),
    Broadcast,
    First,
    Multiple(Vec<Target>),
}

impl Default for Target {
    fn default() -> Self {
        Target::Broadcast
    }
}

#[derive(Clone)]
struct SessionRoute {
    sid: u64,
    sender: InputSender,
    thread_id: Option<u64>,
}

impl SessionRoute {
    fn wire_command(&self, command: &str) -> String {
        match self.thread_id {
            Some(thread_id) => format!("-thread-select {}\n{}", thread_id, command),
            None => command.to_string(),
        }
    }
}

/// Removes tracker state if dispatch fails, times out, or its caller is cancelled.
struct TrackingGuard<'a> {
    tracker: &'a Tracker,
    token: u64,
    armed: bool,
}

impl TrackingGuard<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TrackingGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.tracker.cancel_cmd(self.token);
        }
    }
}

pub struct Router {
    sessions: DashMap<u64, InputSender>,
    tracker: Arc<Tracker>,
}

impl Router {
    pub fn new(tracker: Arc<Tracker>) -> Self {
        Self {
            sessions: DashMap::new(),
            tracker,
        }
    }

    pub fn add_session(&self, sid: u64, session_input_tx: InputSender) {
        self.sessions.insert(sid, session_input_tx);
    }

    pub fn remove_session(&self, sid: u64) {
        self.sessions.remove(&sid);
    }

    fn session_route(&self, sid: u64, thread_id: Option<u64>) -> Result<SessionRoute> {
        let sender = self
            .sessions
            .get(&sid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| anyhow!("Session {} does not exist", sid))?;
        Ok(SessionRoute {
            sid,
            sender,
            thread_id,
        })
    }

    fn session_set_routes(
        &self,
        sids: &HashSet<u64>,
        empty_error: impl FnOnce() -> anyhow::Error,
    ) -> Result<Vec<SessionRoute>> {
        let routes = sids
            .iter()
            .filter_map(|sid| self.session_route(*sid, None).ok())
            .collect::<Vec<_>>();
        if routes.is_empty() {
            return Err(empty_error());
        }
        Ok(routes)
    }

    /// Resolve a target to a stable sender snapshot before registering response
    /// tracking. The snapshot size is therefore the exact expected response count.
    fn resolve_target(&self, target: &Target) -> Result<Vec<SessionRoute>> {
        match target {
            Target::Session(sid) => {
                let route = self.session_route(*sid, None)?;
                STATES.select_session(*sid);
                Ok(vec![route])
            }
            Target::Thread(gtid) => {
                let LocalThreadId(sid, thread_id) = STATES
                    .local_thread_id(*gtid)
                    .ok_or_else(|| anyhow!("Thread (gtid: {}) is not in a session group", gtid))?;
                let route = self.session_route(sid, Some(thread_id))?;
                STATES.select_thread_context(sid, *gtid);
                Ok(vec![route])
            }
            Target::Group(gid) => {
                let group = get_group_mgr()
                    .group_by_id(*gid)
                    .ok_or_else(|| anyhow!("Group (id: {}) doesn't exist", gid))?;
                let session_ids = group.session_ids().clone();
                drop(group);
                self.session_set_routes(&session_ids, || {
                    anyhow!("No live sessions matched group {}", gid)
                })
            }
            Target::CurrThread => {
                let gtid = STATES.current_thread_id().ok_or_else(|| {
                    anyhow!("use -thread-select #gtid to select the thread first.")
                })?;
                self.resolve_target(&Target::Thread(gtid))
            }
            Target::CurrSession => {
                let sid = STATES
                    .current_session_id()
                    .ok_or_else(|| anyhow!("No current session selected."))?;
                self.resolve_target(&Target::Session(sid))
            }
            Target::SessionSet(sids) => {
                self.session_set_routes(sids, || anyhow!("No live sessions matched the target set"))
            }
            Target::Broadcast => {
                let routes = self
                    .sessions
                    .iter()
                    .map(|entry| SessionRoute {
                        sid: *entry.key(),
                        sender: entry.value().clone(),
                        thread_id: None,
                    })
                    .collect::<Vec<_>>();
                if routes.is_empty() {
                    bail!("No active sessions available for broadcast target");
                }
                Ok(routes)
            }
            Target::First => {
                let route = self
                    .sessions
                    .iter()
                    .next()
                    .map(|entry| SessionRoute {
                        sid: *entry.key(),
                        sender: entry.value().clone(),
                        thread_id: None,
                    })
                    .ok_or_else(|| anyhow!("No session available."))?;
                STATES.select_session(route.sid);
                Ok(vec![route])
            }
            Target::Multiple(_) => unreachable!("multiple targets are dispatched individually"),
        }
    }

    fn dispatch<F: DynFormatter + Clone>(
        &self,
        routes: Vec<SessionRoute>,
        cmd: Command<F>,
        output: OutputSource,
    ) -> Result<()> {
        let tracked = !matches!(&output, OutputSource::DISCARD);
        let token = cmd.internal_token;
        let (outgoing, command) = cmd.prepare_to_send(routes.len() as u32, output);

        let mut tracking = tracked.then(|| {
            self.tracker.add_cmd(outgoing);
            TrackingGuard {
                tracker: &self.tracker,
                token,
                armed: true,
            }
        });

        for route in routes {
            debug!(
                "Router writing to session: {}, command: {}",
                route.sid, command
            );
            route
                .sender
                .try_send(route.wire_command(&command).into())
                .map_err(|error| {
                    anyhow!(
                        "Session {} command queue rejected token {}: {}",
                        route.sid,
                        token,
                        error
                    )
                })?;
        }

        if let Some(tracking) = &mut tracking {
            // Responses now own completion of the tracker entry.
            tracking.disarm();
        }
        Ok(())
    }

    async fn dispatch_and_wait<F: DynFormatter + Clone>(
        &self,
        routes: Vec<SessionRoute>,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        let token = cmd.internal_token;
        let (return_tx, return_rx) = tokio::sync::oneshot::channel();
        let (outgoing, command) =
            cmd.prepare_to_send(routes.len() as u32, OutputSource::RETURN(return_tx));
        self.tracker.add_cmd(outgoing);
        let _tracking = TrackingGuard {
            tracker: &self.tracker,
            token,
            armed: true,
        };

        for route in routes {
            debug!(
                "Router writing to session: {}, command: {}",
                route.sid, command
            );
            timeout(
                COMMAND_SEND_TIMEOUT,
                route.sender.send_async(route.wire_command(&command).into()),
            )
            .await
            .map_err(|_| {
                anyhow!(
                    "Timed out enqueueing command token {} for session {}",
                    token,
                    route.sid
                )
            })?
            .map_err(|error| {
                anyhow!(
                    "Failed to enqueue command token {} for session {}: {}",
                    token,
                    route.sid,
                    error
                )
            })?;
        }

        timeout(COMMAND_TIMEOUT, return_rx)
            .await
            .map_err(|_| anyhow!("Timed out waiting for command token {}", token))?
            .map_err(|error| anyhow!("Command token {} was cancelled: {}", token, error))
    }

    pub fn send_to<F: DynFormatter + Clone>(&self, target: Target, cmd: Command<F>) -> Result<()> {
        if let Target::Multiple(targets) = target {
            if targets.is_empty() {
                bail!("No targets provided for multiple target");
            }
            for target in targets {
                self.send_to(target, cmd.clone().with_fresh_internal_token())?;
            }
            return Ok(());
        }
        let routes = self.resolve_target(&target)?;
        self.dispatch(routes, cmd, OutputSource::STDOUT)
    }

    pub async fn send_to_ret<F: DynFormatter + Clone>(
        &self,
        target: Target,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        if let Target::Multiple(targets) = target {
            if targets.is_empty() {
                bail!("No targets provided for multiple target");
            }
            let futures = targets.into_iter().map(|target| {
                Box::pin(self.send_to_ret(target, cmd.clone().with_fresh_internal_token()))
            });
            let results = futures::future::join_all(futures)
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()?;
            let mut results = results.into_iter();
            let mut combined = results
                .next()
                .ok_or_else(|| anyhow!("No results from multiple target"))?;
            for result in results {
                combined
                    .get_responses_mut()
                    .extend(result.get_responses().clone());
            }
            return Ok(combined);
        }
        let routes = self.resolve_target(&target)?;
        self.dispatch_and_wait(routes, cmd).await
    }

    pub fn send_to_and_forget(&self, target: Target, cmd: Command<NullFormatter>) -> Result<()> {
        if let Target::Multiple(targets) = target {
            if targets.is_empty() {
                bail!("No targets provided for multiple target");
            }
            for target in targets {
                self.send_to_and_forget(target, cmd.clone().with_fresh_internal_token())?;
            }
            return Ok(());
        }
        let routes = self.resolve_target(&target)?;
        self.dispatch(routes, cmd, OutputSource::DISCARD)
    }

    pub fn handle_internal_cmd(&self, cmd: &str) {
        if cmd == "p-session-meta" {
            info!("p-session-meta: {:?}", STATES.sessions())
        }

        if cmd == "p-group-mgr" {
            info!("p-group-mgr: {:#?}", get_group_mgr())
        }

        if cmd == "p-source-mgr" {
            info!("p-source-mgr: {:#?}", get_source_mgr())
        }

        if cmd == "p-bkpt-mgr" {
            info!("p-bkpt-mgr: {:#?}", get_bkpt_mgr())
        }

        if cmd == "p-proclet-mgr" {
            info!("p-proclet-mgr: {:#?}", get_proclet_mgr())
        }

        if cmd.contains("p-resolve-src") {
            let parts = cmd.split_whitespace().collect::<Vec<&str>>();
            if parts.len() < 2 {
                info!("Usage: p-resolve-src <source_path>");
                return;
            }
            let path = parts[1].to_string();
            tokio::spawn(async move {
                match get_source_mgr().resolve_src_by_path(&path).await {
                    Ok(_) => {
                        debug!("Source files resolved successfully.");
                    }
                    Err(e) => {
                        debug!("Failed to resolve source files: {:?}", e);
                    }
                }
            });
        }

        if cmd.contains("s-cmd") {
            let parts = cmd.split_whitespace().collect::<Vec<&str>>();
            if parts.len() < 3 {
                info!("Usage: s-cmd <session_id> <cmd>");
                return;
            }
            let sid = parts[1].parse::<u64>().unwrap();
            let cmd_to_send = parts[2..].join(" ");
            let parsed: Result<ParsedInputCmd> = cmd_to_send.clone().try_into();
            if let Ok(parsed) = parsed {
                let (_, command) = parsed.to_command(PlainFormatter);
                if let Err(error) = self.send_to(Target::Session(sid), command) {
                    warn!("Failed to send command: {:?}", error);
                }
            } else {
                warn!("Failed to parse command: {:?}", cmd);
            }
        }

        if cmd.contains("q-proclet") {
            let parts = cmd.split_whitespace().collect::<Vec<&str>>();
            if parts.len() < 2 {
                info!("Usage: q-proclet <proclet_id>");
                return;
            }
            let proclet_id = parts[1].parse::<u64>().unwrap();

            tokio::spawn(async move {
                match get_dbg_mgr().query_proclet(proclet_id).await {
                    Ok(proclet) => {
                        info!("Proclet: {:?}", proclet);
                    }
                    Err(e) => {
                        error!("Failed to query proclet: {:?}", e);
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(token: u64) -> Command<PlainFormatter> {
        Command::new(None, token, "-thread-info".to_string(), PlainFormatter)
    }

    #[test]
    fn broadcast_tracks_the_resolved_session_snapshot() {
        let tracker = Tracker::new();
        let router = Router::new(Arc::clone(&tracker));
        let (first_tx, _first_rx) = flume::bounded(1);
        let (second_tx, _second_rx) = flume::bounded(1);
        router.add_session(1, first_tx);
        router.add_session(2, second_tx);

        router
            .send_to(Target::Broadcast, command(100))
            .expect("broadcast should be enqueued");

        let inflight = tracker.get_inflight_cmds_copy();
        assert_eq!(inflight.len(), 1);
        assert_eq!(inflight[0].target_num_resp, 2);
    }

    #[test]
    fn rejected_dispatch_removes_tracker_state() {
        let tracker = Tracker::new();
        let router = Router::new(Arc::clone(&tracker));
        let (tx, rx) = flume::bounded(1);
        drop(rx);
        router.add_session(1, tx);

        let result = router.send_to(Target::Broadcast, command(101));

        assert!(result.is_err());
        assert!(tracker.get_inflight_cmds_copy().is_empty());
    }

    #[tokio::test]
    async fn cancelling_waited_dispatch_removes_tracker_state() {
        let tracker = Tracker::new();
        let router = Arc::new(Router::new(Arc::clone(&tracker)));
        let (tx, _rx) = flume::bounded(1);
        router.add_session(1, tx);

        let task = tokio::spawn({
            let router = Arc::clone(&router);
            async move { router.send_to_ret(Target::Broadcast, command(102)).await }
        });

        for _ in 0..10 {
            if tracker.get_inflight_cmds_copy().len() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(tracker.get_inflight_cmds_copy().len(), 1);

        task.abort();
        let _ = task.await;
        assert!(tracker.get_inflight_cmds_copy().is_empty());
    }
}
