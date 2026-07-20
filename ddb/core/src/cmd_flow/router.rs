use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Result};
use dashmap::DashMap;
use futures::future::join_all;
use serde::Deserialize;
use tokio::time::timeout;

use super::{
    input::Command,
    response::{FinishedCmd, SessionRuntimeStatus},
    session_runtime::{
        SessionCommand, SessionHandle, SessionLease, SessionTicket, COMMAND_TIMEOUT,
    },
};
use crate::{
    runtime_model::RuntimeModel,
    state::{GroupId, LocalThreadId},
};

const COMMAND_SEND_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Target {
    /// No target was supplied by the command text or caller. Ingress adapters
    /// must resolve this explicitly before routing.
    Unspecified,
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
        Self::Unspecified
    }
}

#[derive(Clone)]
struct SessionRoute {
    handle: SessionHandle,
    thread_id: Option<u64>,
}

impl SessionRoute {
    fn command(&self, command: &Command, token: u64) -> SessionCommand {
        let wire_command = match self.thread_id {
            Some(thread_id) => rewrite_thread_argument(&command.raw_cmd, thread_id),
            None => command.raw_cmd.clone(),
        };
        SessionCommand {
            token,
            command: wire_command,
            thread_id: self.thread_id,
            consistency: command.consistency,
        }
    }
}

pub struct Router {
    model: Arc<RuntimeModel>,
    sessions: DashMap<u64, SessionHandle>,
}

impl Router {
    pub fn new(model: Arc<RuntimeModel>) -> Self {
        Self {
            model,
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
            Target::Unspecified => bail!("command target was not resolved before routing"),
            Target::Session(sid) => Ok(vec![self.session_route(*sid, None)?]),
            Target::Thread(gtid) => {
                let LocalThreadId(sid, thread_id) = self
                    .model
                    .state()
                    .local_thread_id(*gtid)
                    .ok_or_else(|| anyhow!("Thread {} is not in a session", gtid))?;
                Ok(vec![self.session_route(sid, Some(thread_id))?])
            }
            Target::Group(gid) => {
                let group = self
                    .model
                    .groups()
                    .group_by_id(*gid)
                    .ok_or_else(|| anyhow!("Group {} does not exist", gid))?;
                let session_ids = group.session_ids().clone();
                drop(group);
                self.session_set_routes(&session_ids, || {
                    anyhow!("No live sessions matched group {}", gid)
                })
            }
            Target::CurrThread => {
                let gtid = self.model.state().current_thread_id().ok_or_else(|| {
                    anyhow!("use -thread-select #gtid to select the thread first")
                })?;
                self.resolve_target(&Target::Thread(gtid))
            }
            Target::CurrSession => {
                let sid = self
                    .model
                    .state()
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
        let route = routes
            .pop()
            .expect("exclusive target has exactly one route");
        let session_command = route.command(&command, command.internal_token);
        let response = timeout(COMMAND_TIMEOUT, lease.execute(session_command))
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
}

fn rewrite_thread_argument(command: &str, local_thread_id: u64) -> String {
    let mut parts = command
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(index) = parts.iter().position(|part| *part == "--thread") {
        if let Some(thread_id) = parts.get_mut(index + 1) {
            *thread_id = local_thread_id.to_string();
            return parts.join(" ");
        }
    }
    command.to_string()
}

#[cfg(test)]
mod tests {
    use super::rewrite_thread_argument;

    #[test]
    fn thread_argument_is_localized_at_the_route_boundary() {
        assert_eq!(
            rewrite_thread_argument("-stack-list-frames --thread 9001 0 5", 7),
            "-stack-list-frames --thread 7 0 5"
        );
        assert_eq!(
            rewrite_thread_argument("-exec-interrupt", 7),
            "-exec-interrupt"
        );
    }
}
