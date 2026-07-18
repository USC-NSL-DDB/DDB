use std::{collections::HashSet, time::Duration};

use anyhow::{anyhow, bail, Result};
use dashmap::DashMap;
use futures::future::join_all;
use serde::Deserialize;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::{
    emit,
    input::{Command, ParsedInputCmd},
    response::{FinishedCmd, SessionRuntimeStatus},
    session_runtime::{
        SessionCommand, SessionHandle, SessionLease, SessionTicket, COMMAND_TIMEOUT,
    },
    DynFormatter, PlainFormatter,
};
use crate::{
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
        Self::Broadcast
    }
}

#[derive(Clone)]
struct SessionRoute {
    handle: SessionHandle,
    thread_id: Option<u64>,
}

impl SessionRoute {
    fn command(&self, command: &Command, token: u64) -> SessionCommand {
        SessionCommand {
            token,
            command: command.raw_cmd.clone(),
            thread_id: self.thread_id,
            consistency: command.consistency,
        }
    }
}

pub struct Router {
    sessions: DashMap<u64, SessionHandle>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    pub fn add_session(&self, handle: SessionHandle) {
        self.sessions.insert(handle.sid(), handle);
    }

    pub fn remove_session(&self, sid: u64) {
        self.sessions.remove(&sid);
    }

    pub fn session_handle(&self, sid: u64) -> Result<SessionHandle> {
        self.sessions
            .get(&sid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| anyhow!("Session {} does not exist", sid))
    }

    fn session_route(&self, sid: u64, thread_id: Option<u64>) -> Result<SessionRoute> {
        Ok(SessionRoute {
            handle: self.session_handle(sid)?,
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

    fn resolve_target(&self, target: &Target) -> Result<Vec<SessionRoute>> {
        match target {
            Target::Session(sid) => {
                STATES.select_session(*sid);
                Ok(vec![self.session_route(*sid, None)?])
            }
            Target::Thread(gtid) => {
                let LocalThreadId(sid, thread_id) = STATES
                    .local_thread_id(*gtid)
                    .ok_or_else(|| anyhow!("Thread {} is not in a session", gtid))?;
                STATES.select_thread_context(sid, *gtid);
                Ok(vec![self.session_route(sid, Some(thread_id))?])
            }
            Target::Group(gid) => {
                let group = get_group_mgr()
                    .group_by_id(*gid)
                    .ok_or_else(|| anyhow!("Group {} does not exist", gid))?;
                let session_ids = group.session_ids().clone();
                drop(group);
                self.session_set_routes(&session_ids, || {
                    anyhow!("No live sessions matched group {}", gid)
                })
            }
            Target::CurrThread => {
                let gtid = STATES.current_thread_id().ok_or_else(|| {
                    anyhow!("use -thread-select #gtid to select the thread first")
                })?;
                self.resolve_target(&Target::Thread(gtid))
            }
            Target::CurrSession => {
                let sid = STATES
                    .current_session_id()
                    .ok_or_else(|| anyhow!("No current session selected"))?;
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
                        handle: entry.value().clone(),
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
                        handle: entry.value().clone(),
                        thread_id: None,
                    })
                    .ok_or_else(|| anyhow!("No session available"))?;
                STATES.select_session(route.handle.sid());
                Ok(vec![route])
            }
            Target::Multiple(targets) => {
                if targets.is_empty() {
                    bail!("No targets provided for multiple target");
                }
                let mut routes = Vec::new();
                for target in targets {
                    routes.extend(self.resolve_target(target)?);
                }
                Ok(routes)
            }
        }
    }

    async fn submit_routes(
        &self,
        routes: Vec<SessionRoute>,
        command: &Command,
    ) -> Result<Vec<SessionTicket>> {
        let submissions = routes.into_iter().enumerate().map(|(index, route)| {
            let token = if index == 0 {
                command.internal_token
            } else {
                crate::common::counter::next_token()
            };
            let session_command = route.command(command, token);
            async move {
                let sid = route.handle.sid();
                timeout(COMMAND_SEND_TIMEOUT, route.handle.submit(session_command))
                    .await
                    .map_err(|_| anyhow!("Timed out admitting command for session {}", sid))?
                    .map_err(|error| anyhow!("Session {} rejected command: {}", sid, error))
            }
        });
        join_all(submissions).await.into_iter().collect()
    }

    async fn collect(
        external_token: Option<u64>,
        tickets: Vec<SessionTicket>,
    ) -> Result<FinishedCmd> {
        let completions = tickets.into_iter().map(SessionTicket::complete);
        let mut responses = timeout(COMMAND_TIMEOUT, async {
            join_all(completions)
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()
        })
        .await
        .map_err(|_| anyhow!("Timed out waiting for command responses"))??;
        responses.sort_by_key(|response| response.get_sid());
        let sid = responses
            .first()
            .map(|response| response.get_sid())
            .unwrap_or(0);
        Ok(FinishedCmd::new(external_token, sid, responses))
    }

    pub async fn execute(&self, target: Target, command: Command) -> Result<FinishedCmd> {
        let routes = self.resolve_target(&target)?;
        let tickets = self.submit_routes(routes, &command).await?;
        Self::collect(command.external_token, tickets).await
    }

    pub async fn emit<F>(&self, target: Target, command: Command, formatter: F) -> Result<()>
    where
        F: DynFormatter + 'static,
    {
        let routes = self.resolve_target(&target)?;
        let tickets = self.submit_routes(routes, &command).await?;
        tokio::spawn(async move {
            match Self::collect(command.external_token, tickets).await {
                Ok(finished) => emit(finished, Box::new(formatter)),
                Err(error) => warn!(?error, "detached command failed"),
            }
        });
        Ok(())
    }

    pub async fn submit(&self, target: Target, command: Command) -> Result<()> {
        let routes = self.resolve_target(&target)?;
        let tickets = self.submit_routes(routes, &command).await?;
        drop(tickets);
        Ok(())
    }

    pub async fn execute_exclusive(
        &self,
        lease: &SessionLease,
        target: Target,
        command: Command,
    ) -> Result<FinishedCmd> {
        let mut routes = self.resolve_target(&target)?;
        if routes.len() != 1 || routes[0].handle.sid() != lease.sid() {
            bail!(
                "exclusive command target must resolve to session {}",
                lease.sid()
            );
        }
        let thread_id = routes.pop().and_then(|route| route.thread_id);
        let response = timeout(
            COMMAND_TIMEOUT,
            lease.execute(SessionCommand {
                token: command.internal_token,
                command: command.raw_cmd,
                thread_id,
                consistency: command.consistency,
            }),
        )
        .await
        .map_err(|_| anyhow!("Timed out waiting for exclusive command"))??;
        Ok(FinishedCmd::new(
            command.external_token,
            lease.sid(),
            vec![response],
        ))
    }

    pub fn runtime_statuses(&self) -> Vec<SessionRuntimeStatus> {
        let mut statuses = self
            .sessions
            .iter()
            .map(|entry| entry.value().status())
            .collect::<Vec<_>>();
        statuses.sort_by_key(|status| status.sid);
        statuses
    }

    pub fn handle_internal_cmd(&self, command: &str) {
        match command {
            "p-session-meta" => info!("p-session-meta: {:?}", STATES.sessions()),
            "p-group-mgr" => info!("p-group-mgr: {:#?}", get_group_mgr()),
            "p-source-mgr" => info!("p-source-mgr: {:#?}", get_source_mgr()),
            "p-bkpt-mgr" => info!("p-bkpt-mgr: {:#?}", get_bkpt_mgr()),
            "p-proclet-mgr" => info!("p-proclet-mgr: {:#?}", get_proclet_mgr()),
            _ if command.starts_with("p-resolve-src ") => {
                let path = command["p-resolve-src ".len()..].to_string();
                tokio::spawn(async move {
                    if let Err(error) = get_source_mgr().resolve_src_by_path(&path).await {
                        debug!(?error, "failed to resolve source path");
                    }
                });
            }
            _ if command.starts_with("s-cmd ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                if parts.len() < 3 {
                    info!("Usage: s-cmd <session_id> <cmd>");
                    return;
                }
                let Ok(sid) = parts[1].parse::<u64>() else {
                    warn!("Invalid session id: {}", parts[1]);
                    return;
                };
                let raw = parts[2..].join(" ");
                tokio::spawn(async move {
                    let parsed: Result<ParsedInputCmd> = raw.try_into();
                    match parsed {
                        Ok(parsed) => {
                            let (_, command) = parsed.to_command();
                            if let Err(error) = crate::cmd_flow::get_router()
                                .emit(Target::Session(sid), command, PlainFormatter)
                                .await
                            {
                                warn!(?error, "failed to send internal command");
                            }
                        }
                        Err(error) => warn!(?error, "failed to parse internal command"),
                    }
                });
            }
            _ if command.starts_with("q-proclet ") => {
                let parts = command.split_whitespace().collect::<Vec<_>>();
                if let Some(Ok(proclet_id)) = parts.get(1).map(|id| id.parse::<u64>()) {
                    tokio::spawn(async move {
                        match get_dbg_mgr().query_proclet(proclet_id).await {
                            Ok(proclet) => info!("Proclet: {:?}", proclet),
                            Err(error) => warn!(?error, "failed to query proclet"),
                        }
                    });
                }
            }
            _ => {}
        }
    }
}
