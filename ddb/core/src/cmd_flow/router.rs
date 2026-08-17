use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Result};
use dashmap::DashMap;
use futures::future::join_all;
use serde::Deserialize;
use tokio::{sync::broadcast, time::timeout};

use super::{
    input::Command,
    response::{FinishedCmd, SessionRuntimeStatus},
    session_runtime::{
        PendingCommandChange, SessionCommand, SessionHandle, SessionLease, SessionPendingCommand,
        SessionTicket, COMMAND_TIMEOUT,
    },
};
use crate::{
    state::RuntimeModel,
    state::{GlobalThreadId, GroupId, LocalThreadId},
};

const COMMAND_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const PENDING_CHANGE_CAPACITY: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionCommandFailureKind {
    AdmissionTimeout,
    AdmissionRejected,
    ResponseTimeout,
    ResponseFailed,
    DebuggerRejected,
    ExecutionFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionCommandFailure {
    sid: u64,
    kind: SessionCommandFailureKind,
}

impl SessionCommandFailure {
    pub(crate) fn new(sid: u64, kind: SessionCommandFailureKind) -> Self {
        Self { sid, kind }
    }

    pub(crate) fn sid(&self) -> u64 {
        self.sid
    }

    pub(crate) fn kind(&self) -> SessionCommandFailureKind {
        self.kind
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CommandFanoutReport {
    completion: FinishedCmd,
    failures: Vec<SessionCommandFailure>,
}

impl CommandFanoutReport {
    pub(crate) fn new(
        external_token: Option<u64>,
        mut responses: Vec<super::ParsedSessionResponse>,
        mut failures: Vec<SessionCommandFailure>,
    ) -> Self {
        responses.sort_by_key(|response| response.get_sid());
        failures.sort_unstable_by_key(SessionCommandFailure::sid);
        let sid = responses
            .first()
            .map(|response| response.get_sid())
            .unwrap_or(0);
        Self {
            completion: FinishedCmd::new(external_token, sid, responses),
            failures,
        }
    }

    pub(crate) fn completion(&self) -> &FinishedCmd {
        &self.completion
    }

    pub(crate) fn failures(&self) -> &[SessionCommandFailure] {
        &self.failures
    }

    pub(crate) fn into_result(self) -> Result<FinishedCmd> {
        if self.failures.is_empty() {
            Ok(self.completion)
        } else {
            Err(CommandFanoutError { report: self }.into())
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("debugger command failed for one or more session targets")]
pub(crate) struct CommandFanoutError {
    report: CommandFanoutReport,
}

impl CommandFanoutError {
    pub(crate) fn report(&self) -> &CommandFanoutReport {
        &self.report
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub enum Target {
    /// No target was supplied by the command text or caller. Ingress adapters
    /// must resolve this explicitly before routing.
    Unspecified,
    Session(u64),
    Thread(GlobalThreadId),
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
    fn command(&self, command: &Command) -> SessionCommand {
        let wire_command = match self.thread_id {
            Some(thread_id) => rewrite_thread_argument(&command.raw_cmd, thread_id),
            None => command.raw_cmd.clone(),
        };
        SessionCommand {
            command: wire_command,
            thread_id: self.thread_id,
            consistency: command.consistency,
            metadata: command.metadata.clone(),
        }
    }
}

pub struct Router {
    model: Arc<RuntimeModel>,
    sessions: DashMap<u64, SessionHandle>,
    pending_changes: broadcast::Sender<PendingCommandChange>,
}

impl Router {
    pub fn new(model: Arc<RuntimeModel>) -> Self {
        let (pending_changes, _) = broadcast::channel(PENDING_CHANGE_CAPACITY);
        Self {
            model,
            sessions: DashMap::new(),
            pending_changes,
        }
    }

    pub fn add_session(&self, handle: SessionHandle) {
        let sid = handle.sid();
        if let Some(previous) = self.sessions.insert(sid, handle.clone()) {
            previous.detach_pending_events();
        }
        handle.attach_pending_events(self.pending_changes.clone());
    }

    pub fn remove_session(&self, sid: u64) {
        if let Some((_, handle)) = self.sessions.remove(&sid) {
            handle.detach_pending_events();
            let _ = self.pending_changes.send(PendingCommandChange::Reconcile);
        }
    }

    pub(crate) fn subscribe_pending_changes(&self) -> broadcast::Receiver<PendingCommandChange> {
        self.pending_changes.subscribe()
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
            .map(|sid| self.session_route(*sid, None))
            .collect::<Result<Vec<_>>>()?;
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
                    .local_thread_id(*gtid)
                    .ok_or_else(|| anyhow!("Thread {} is not in a session", gtid))?;
                Ok(vec![self.session_route(sid, Some(thread_id))?])
            }
            Target::Group(gid) => {
                let group = self
                    .model
                    .group_by_id(*gid)
                    .ok_or_else(|| anyhow!("Group {} does not exist", gid))?;
                let session_ids = group.session_ids().clone();
                drop(group);
                self.session_set_routes(&session_ids, || {
                    anyhow!("No live sessions matched group {}", gid)
                })
            }
            Target::CurrThread => {
                let gtid = self.model.current_thread_id().ok_or_else(|| {
                    anyhow!("use -thread-select #gtid to select the thread first")
                })?;
                self.resolve_target(&Target::Thread(gtid))
            }
            Target::CurrSession => {
                let sid = self
                    .model
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

    pub(crate) fn resolve_session_ids(&self, target: &Target) -> Result<Vec<u64>> {
        let mut session_ids = self
            .resolve_target(target)?
            .into_iter()
            .map(|route| route.handle.sid())
            .collect::<Vec<_>>();
        session_ids.sort_unstable();
        session_ids.dedup();
        Ok(session_ids)
    }

    async fn submit_routes(
        &self,
        routes: Vec<SessionRoute>,
        command: &Command,
    ) -> (Vec<SessionTicket>, Vec<SessionCommandFailure>) {
        let submissions = routes.into_iter().map(|route| {
            let session_command = route.command(command);
            async move {
                let sid = route.handle.sid();
                match timeout(COMMAND_SEND_TIMEOUT, route.handle.submit(session_command)).await {
                    Ok(Ok(ticket)) => Ok(ticket),
                    Ok(Err(_)) => Err(SessionCommandFailure::new(
                        sid,
                        SessionCommandFailureKind::AdmissionRejected,
                    )),
                    Err(_) => Err(SessionCommandFailure::new(
                        sid,
                        SessionCommandFailureKind::AdmissionTimeout,
                    )),
                }
            }
        });
        let mut tickets = Vec::new();
        let mut failures = Vec::new();
        for result in join_all(submissions).await {
            match result {
                Ok(ticket) => tickets.push(ticket),
                Err(failure) => failures.push(failure),
            }
        }
        (tickets, failures)
    }

    async fn collect(
        external_token: Option<u64>,
        tickets: Vec<SessionTicket>,
        mut failures: Vec<SessionCommandFailure>,
    ) -> Result<FinishedCmd> {
        let completions = tickets.into_iter().map(|ticket| async move {
            let sid = ticket.sid();
            match timeout(COMMAND_TIMEOUT, ticket.complete()).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(_)) => Err(SessionCommandFailure::new(
                    sid,
                    SessionCommandFailureKind::ResponseFailed,
                )),
                Err(_) => Err(SessionCommandFailure::new(
                    sid,
                    SessionCommandFailureKind::ResponseTimeout,
                )),
            }
        });
        let mut responses = Vec::new();
        for result in join_all(completions).await {
            match result {
                Ok(response) => responses.push(response),
                Err(failure) => failures.push(failure),
            }
        }
        CommandFanoutReport::new(external_token, responses, failures).into_result()
    }

    pub async fn execute(&self, target: Target, command: Command) -> Result<FinishedCmd> {
        let routes = self.resolve_target(&target)?;
        let (tickets, failures) = self.submit_routes(routes, &command).await;
        Self::collect(command.external_token, tickets, failures).await
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
        let session_command = route.command(&command);
        let response = match timeout(COMMAND_TIMEOUT, lease.execute(session_command)).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                return CommandFanoutReport::new(
                    command.external_token,
                    Vec::new(),
                    vec![SessionCommandFailure::new(
                        lease.sid(),
                        SessionCommandFailureKind::ResponseFailed,
                    )],
                )
                .into_result();
            }
            Err(_) => {
                return CommandFanoutReport::new(
                    command.external_token,
                    Vec::new(),
                    vec![SessionCommandFailure::new(
                        lease.sid(),
                        SessionCommandFailureKind::ResponseTimeout,
                    )],
                )
                .into_result();
            }
        };
        CommandFanoutReport::new(command.external_token, vec![response], Vec::new()).into_result()
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

    pub(crate) fn pending_commands(&self) -> Vec<SessionPendingCommand> {
        let mut commands = self
            .sessions
            .iter()
            .flat_map(|entry| entry.value().pending_commands())
            .collect::<Vec<_>>();
        commands.sort_unstable_by_key(|command| (command.sid, command.token));
        commands
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
    use super::{rewrite_thread_argument, Command, Router, Target};
    use crate::{cmd_flow::session_runtime::CompletionConsistency, state::RuntimeModel};

    #[tokio::test]
    async fn unresolved_targets_are_rejected_at_the_routing_boundary() {
        let router = Router::new(RuntimeModel::new());
        let command = Command::new(
            None,
            "-thread-info".to_string(),
            CompletionConsistency::StateConsistent,
        );

        let error = router
            .execute(Target::Unspecified, command)
            .await
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("command target was not resolved before routing"));
    }

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
