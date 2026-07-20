use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use gdbmi::raw::{Dict, Value};
use tokio::task::JoinHandle;
use tracing::{debug, error};

use crate::{
    cmd_flow::transaction,
    common::Config,
    debugger::get_debugger_backend,
    feature::get_proclet_restore_mgr,
    state::{get_state_mgr, LocalThreadId, SessionRef, ThreadContext, ThreadStatus, STATES},
};

use super::{
    api, breakpoint::BreakpointService, decoder::Payload,
    framework_adapter::FrameworkCommandAdapter, input::ParsedInputCmd, query::QueryProjector,
    router::Target, CommandOutcome, DebuggerDataErr, FinishedCmd, ParsedSessionResponse,
    Presentation,
};

/// Command operation selected by the engine after parsing and target resolution.
///
/// # Contract Semantics
///
/// Implementors of this trait are responsible for:
/// 1. **Preserving Target Semantics**: The target from `ParsedInputCmd` must be honored
///    unless the handler has specific routing requirements (e.g., `ThreadSelectHandler`
///    may adjust targets for thread selection commands)
///
/// 2. **Async Execution**: All command processing is async to support routing operations
///    that may involve network I/O to distributed debuggee processes.
///
/// 3. **Structured Completion**: Handlers return a semantic outcome. They never
///    print, format an error, or detach work; those are ingress responsibilities.
#[async_trait]
pub trait Handler: Send + Sync + std::fmt::Debug {
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome>;
}

#[derive(Debug)]
pub struct DefaultHandler;

impl DefaultHandler {
    pub fn new() -> Self {
        DefaultHandler
    }
}

#[async_trait]
impl Handler for DefaultHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = api::parsed(cmd)?.execute().await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }
}

#[derive(Debug)]
pub struct BreakInsertHandler {
    service: Arc<BreakpointService>,
}

impl BreakInsertHandler {
    pub(crate) fn new(service: Arc<BreakpointService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for BreakInsertHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.insert(cmd).await
    }
}

#[derive(Debug)]
pub struct BreakDeleteHandler {
    service: Arc<BreakpointService>,
}

impl BreakDeleteHandler {
    pub(crate) fn new(service: Arc<BreakpointService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for BreakDeleteHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        self.service.delete(cmd).await
    }
}

#[derive(Debug)]
pub struct ThreadInfoHandler {
    projector: QueryProjector<'static>,
}

impl ThreadInfoHandler {
    pub fn new(state: &'static crate::state::StateMgr) -> Self {
        Self {
            projector: QueryProjector::new(state),
        }
    }
}

#[async_trait]
impl Handler for ThreadInfoHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        match cmd.target {
            Target::Thread(tid) => {
                let (_, local_tid) = self.projector.resolve_thread(tid)?;
                let thrd_info_cmd = format!(
                    "{}-thread-info {}",
                    cmd.external_token
                        .map(|token| { token.to_string() })
                        .unwrap_or("".to_string()),
                    local_tid
                );
                let response = api::command(&thrd_info_cmd)?
                    .target(Target::Thread(tid))
                    .execute()
                    .await?;
                let response = self.projector.project_threads(response)?;
                Ok(CommandOutcome::response(response, Presentation::ThreadInfo))
            }
            _ => {
                let response = api::parsed(cmd)?.execute().await?;
                let response = self.projector.project_threads(response)?;
                Ok(CommandOutcome::response(response, Presentation::ThreadInfo))
            }
        }
    }
}

#[derive(Debug)]
pub struct ContinueHandler;

impl ContinueHandler {
    pub fn new() -> Self {
        ContinueHandler
    }
}

impl ContinueHandler {
    #[cfg_attr(feature = "profile", tracing::instrument)]
    async fn continue_session(
        cont_cmd: ParsedInputCmd,
        session: SessionRef,
    ) -> Result<FinishedCmd> {
        let sid = session.read_with(|meta| meta.sid()).await;
        let tx = transaction::begin(sid)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let (sid, in_custom_context, current_context) = tx
            .session()
            .read_with(|meta| {
                (
                    meta.sid(),
                    meta.is_in_custom_context(),
                    meta.current_context().cloned(),
                )
            })
            .await;

        if in_custom_context {
            let Some(ctx) = current_context else {
                bail!("Session {} has no context to restore", sid);
            };
            let restore = api::command(&format!(
                "-switch-context-custom {}",
                Self::prepare_ctx_switch_args(&ctx)
            ))?
            .target(Target::Thread(ctx.tid))
            .execute_exclusive(tx.lease())
            .await?;
            let restored = restore.get_responses().len() == 1
                && Payload::first(&restore)?.string("message")? == "success";

            session
                .write_with(|meta| meta.set_in_custom_context(!restored))
                .await;

            if !restored {
                bail!("Failed to restore context for session {}", sid);
            }
        }

        let response = api::parsed(cont_cmd)?
            .target(Target::Session(sid))
            .execute_exclusive(tx.lease())
            .await?;
        session
            .write_with(|meta| meta.update_all_status(ThreadStatus::RUNNING))
            .await;
        Ok(response)
    }

    #[inline]
    fn prepare_ctx_switch_args(regs: &ThreadContext) -> String {
        regs.ctx
            .iter()
            .fold(format!(""), |acc, (reg, val)| {
                format!("{} {}={}", acc, reg, val)
            })
            .trim()
            .to_string()
    }
}

#[async_trait]
impl Handler for ContinueHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if Config::global().conf.support_migration {
            // reset all proclet cache and clean up restored proclet heap.
            get_proclet_restore_mgr().reset().await;
        }

        let external_token = cmd.external_token;
        let tasks: Vec<JoinHandle<Result<FinishedCmd>>> = match &cmd.target {
            Target::Session(sid) => get_state_mgr()
                .session(*sid)
                .into_iter()
                .map(|session| {
                    let cmd = cmd.clone();
                    tokio::spawn(async move { Self::continue_session(cmd, session).await })
                })
                .collect(),
            _ => get_state_mgr()
                .sessions()
                .into_iter()
                .map(|session| {
                    let cmd = cmd.clone();
                    tokio::spawn(async move { Self::continue_session(cmd, session).await })
                })
                .collect(),
        };

        let mut responses = Vec::<ParsedSessionResponse>::new();
        for result in futures::future::join_all(tasks).await {
            match result {
                Err(e) => return Err(anyhow!("Continue task failed: {e}")),
                Ok(Err(e)) => return Err(e.context("Failed to continue")),
                Ok(Ok(response)) => responses.extend(response.get_responses().iter().cloned()),
            }
        }
        Ok(CommandOutcome::response(
            FinishedCmd::new(external_token, 0, responses),
            Presentation::Unit,
        ))
    }
}

#[derive(Debug)]
pub struct InterruptHandler;

impl InterruptHandler {
    pub fn new() -> Self {
        InterruptHandler
    }
}

#[async_trait]
impl Handler for InterruptHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let cmd = cmd.with_prefix("-exec-interrupt-if-running");
        match cmd.target {
            Target::Session(sid) => {
                let ss = STATES.session(sid);
                if ss.is_some() {
                    // Note: send interrupt to running process. Ignore thread granularity.
                    // skips checking if the thread is running or not.
                    let response = api::parsed(cmd)?.execute().await?;
                    Ok(CommandOutcome::response(response, Presentation::Plain))
                } else {
                    Ok(CommandOutcome::empty())
                }
            }
            _ => {
                // broadcast to all sessions
                let response = api::parsed(cmd)?
                    .target(Target::Broadcast)
                    .execute()
                    .await?;
                Ok(CommandOutcome::response(response, Presentation::Plain))
            }
        }
    }
}

#[derive(Debug)]
pub struct ListHandler;

impl ListHandler {
    pub fn new() -> Self {
        ListHandler
    }
}

#[async_trait]
impl Handler for ListHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        // FIXME: a naive implementation here, just select the first session
        // This command is need for CLI (to list out sources), but probably not for GUI?
        let response = api::parsed(cmd)?
            .target(Target::Session(1))
            .execute()
            .await?;
        Ok(CommandOutcome::response(response, Presentation::Plain))
    }
}

#[derive(Debug)]
pub struct ThreadSelectHandler;

impl ThreadSelectHandler {
    pub fn new() -> Self {
        ThreadSelectHandler
    }
}

#[async_trait]
impl Handler for ThreadSelectHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let parts = cmd.args.split_whitespace().collect::<Vec<_>>();
        if !parts.is_empty() {
            let gtid = parts.last().unwrap().parse::<u64>()?;
            let (sid, tid) = STATES
                .local_thread_id(gtid)
                .ok_or_else(|| anyhow!("Unknown global thread {}", gtid))?
                .into();
            let target = Target::Session(sid);
            let response = api::command(&format!("-thread-select {}", tid))?
                .target(target)
                .execute()
                .await?;
            Ok(CommandOutcome::response(response, Presentation::Plain))
        } else {
            let response = api::parsed(cmd)?.execute().await?;
            Ok(CommandOutcome::response(response, Presentation::Plain))
        }
    }
}

#[derive(Debug)]
pub struct ListGroupsHandler {
    projector: QueryProjector<'static>,
}

impl ListGroupsHandler {
    pub fn new(state: &'static crate::state::StateMgr) -> Self {
        Self {
            projector: QueryProjector::new(state),
        }
    }
}

#[async_trait]
impl Handler for ListGroupsHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        let response = api::parsed(cmd)?
            .target(Target::Broadcast)
            .execute()
            .await?;
        let response = self.projector.project_processes(response)?;
        Ok(CommandOutcome::response(
            response,
            Presentation::ProcessReadable,
        ))
    }
}

struct BacktraceData {
    bt: FinishedCmd,
    parent_meta: Option<Dict>,
}

#[derive(Debug)]
pub struct DistributeBacktraceHandler {
    adapter: Arc<dyn FrameworkCommandAdapter>,
}

impl DistributeBacktraceHandler {
    pub fn new(adapter: Arc<dyn FrameworkCommandAdapter>) -> Self {
        DistributeBacktraceHandler { adapter }
    }

    fn extract_remote_metadata(&self, payload: &Dict) -> Result<Dict> {
        let meta = payload
            .get("metadata")
            .ok_or(DebuggerDataErr::MissingEntry("metadata".into()))?;

        let msg = payload
            .get("message")
            .ok_or(DebuggerDataErr::MissingEntry("message".into()))?
            .expect_string_ref()?;

        let caller_meta = meta.get_dict_entry("caller_meta")?;
        let caller_ctx = meta.get_dict_entry("caller_ctx")?;

        let pid = caller_meta
            .get_dict_entry("pid")
            .ok()
            .and_then(|v| v.expect_string_repr::<u64>().ok())
            .unwrap_or(0);

        let proclet_id = caller_meta
            .get_dict_entry("proclet_id")
            .ok()
            .and_then(|v| v.clone().expect_string().ok())
            .unwrap_or("".to_string());

        let id = self.adapter.extract_id_from_metadata(caller_meta)?;

        let out_data: Dict = Dict(
            vec![
                ("message".into(), msg.to_string().into()),
                ("caller_ctx".into(), caller_ctx.clone()),
                ("id".into(), id.into()),
                ("pid".into(), pid.to_string().into()),
                ("proclet_id".into(), proclet_id.into()),
            ]
            .into_iter()
            .collect(),
        );
        Ok(out_data)
    }

    fn prepare_ctx_switch_args(regs: &Dict) -> String {
        regs.as_map()
            .iter()
            .fold(format!(""), |acc, (reg, val)| {
                if let Ok(val) = val.expect_string_ref() {
                    format!("{} {}={}", acc, reg, val)
                } else {
                    acc
                }
            })
            .trim()
            .to_string()
    }

    fn extract_ctx_from_payload(payload: &Dict, gtid: u64) -> Result<ThreadContext> {
        let ctx = payload
            .get("old_ctx")
            .ok_or(DebuggerDataErr::MissingEntry("old_ctx".into()))?
            .expect_dict_ref()?;

        let ctx = ctx
            .as_map()
            .iter()
            .map(|(k, v)| {
                let k = k.to_string();
                let v = v.expect_string_repr::<u64>().unwrap();
                (k, v)
            })
            .collect();

        Ok(ThreadContext { ctx, tid: gtid })
    }

    async fn wait_for_all_threads_stopped(session: &SessionRef) -> Result<()> {
        // set deadline to 1s
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        // busy wait for the interrupt to take effect for sure
        // e.g. the thread status is changed to STOPPED
        loop {
            let all_stopped = session
                .read_with(|meta| {
                    debug!("check thread status for {}", meta.tag());
                    meta.all_threads_stopped()
                })
                .await;
            if !all_stopped {
                if std::time::Instant::now() > deadline {
                    bail!("wait too long for interrupt to take effect, break call chain here.");
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            return Ok(());
        }
    }
}

impl DistributeBacktraceHandler {
    async fn get_bt_and_caller_meta_locked(
        &self,
        tx: &transaction::SessionTransaction,
        sid: u64,
        gtid: u64,
    ) -> Result<BacktraceData> {
        let mut stack_resp = api::command(&format!("-stack-list-frames --thread {}", gtid))
            .unwrap()
            .execute_exclusive(tx.lease())
            .await?;

        let stack = Self::get_stack_ref_mut(&mut stack_resp)
            .ok_or(anyhow!("Unable to get stack frames from response"))?;
        for frame in stack.iter_mut() {
            let frame = frame.expect_dict_ref_mut().unwrap();
            frame.insert("session".to_string(), sid.to_string().into());
            frame.insert("thread".to_string(), gtid.to_string().into());
        }

        let dbt_cmd = self.adapter.get_bt_command_name();
        let resp = api::command(&dbt_cmd)
            .unwrap()
            .target(Target::Thread(gtid))
            .execute_exclusive(tx.lease())
            .await
            .unwrap();

        let remote_bt_parent_meta = match self
            .extract_remote_metadata(resp.get_responses().first().unwrap().get_payload().unwrap())
        {
            Ok(meta) => Some(meta),
            Err(e) => {
                debug!("No dbt metadata is found due to: {:?}.", e);
                None
            }
        };
        Ok(BacktraceData {
            bt: stack_resp,
            parent_meta: remote_bt_parent_meta,
        })
    }

    async fn get_bt_and_caller_meta(&self, gtid: u64) -> Result<BacktraceData> {
        // ------------ [BEGIN] get backtrace for the current thread ------------
        let (sid, _) = get_state_mgr().local_thread_id(gtid).unwrap().into();

        // Acquire transaction lock for exclusive command sequence access
        let tx = transaction::begin(sid)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        self.get_bt_and_caller_meta_locked(&tx, sid, gtid).await
    }

    async fn handle_migration_if_enabled(
        &self,
        inspect_gtid: u64,
        parent_meta: &Dict,
        transaction: Option<&transaction::SessionTransaction>,
    ) {
        if Config::global().handle_migration() {
            if let Some(LocalThreadId(sid, _)) = STATES.local_thread_id(inspect_gtid) {
                let proclet_id = parent_meta
                    .get("proclet_id")
                    .unwrap()
                    .expect_string_ref()
                    .unwrap()
                    .to_string();
                match get_proclet_restore_mgr()
                    .handle_proclet_restoration(sid, &proclet_id, transaction)
                    .await
                {
                    Ok(_) => {
                        debug!("proclet heap restoration done for session {}", sid);
                    }
                    Err(e) => {
                        error!("Failed to handle proclet heap restoration: {:?}", e);
                    }
                }
            } else {
                error!(
                    "Failed to handle proclet heap restoration: unable to resolve sid for gtid={}.",
                    inspect_gtid
                );
            }
        }
    }

    // helper functions
    #[allow(unused)]
    fn get_stack_ref<'a>(response: &'a FinishedCmd) -> &'a Vec<Value> {
        response
            .get_responses()
            .first()
            .unwrap()
            .get_payload()
            .unwrap()
            .get("stack")
            .unwrap()
            .expect_list_ref()
            .unwrap()
    }

    fn get_stack_ref_mut<'a>(response: &'a mut FinishedCmd) -> Option<&'a mut Vec<Value>> {
        response
            .get_responses_mut()
            .first_mut()?
            .get_payload_mut()?
            .get_mut("stack")?
            .expect_list_ref_mut()
            .ok()
    }

    fn get_stack_owned(mut response: FinishedCmd) -> Option<Vec<Value>> {
        response
            .get_responses_mut()
            .first_mut()?
            .get_payload_mut()?
            .remove("stack")?
            .expect_list()
            .ok()
    }

    fn add_reordered_frame_levels<'a>(responses: &'a mut FinishedCmd) {
        let stack = match Self::get_stack_ref_mut(responses) {
            Some(s) => s,
            None => {
                debug!(
                    "Unable to read stack information for reordering frame levels: {:?}",
                    responses
                );
                return;
            }
        };
        for (i, frame) in stack.iter_mut().enumerate() {
            let frame = frame.expect_dict_ref_mut().unwrap();
            frame.insert("level_reordered".to_string(), (i as u64).to_string().into());
        }
    }
}

#[async_trait]
impl Handler for DistributeBacktraceHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if let Target::Thread(gtid) = &cmd.target {
            let mut out_result: FinishedCmd;
            let mut inspect_gtid = *gtid;

            // TODO: initialize deadlock detection meta here

            // get current thread backtrace and caller metadata
            let bt_data = self.get_bt_and_caller_meta(inspect_gtid).await;
            let parent_meta = match bt_data {
                Ok(data) => {
                    out_result = data.bt;
                    data.parent_meta
                }
                Err(e) => {
                    let err_msg = format!(
                        "Failed to get backtrace for thread {}, break the call chain: {:?}",
                        inspect_gtid, e
                    );
                    error!(err_msg);
                    return Err(anyhow!(err_msg));
                }
            };
            if let Some(external_token) = cmd.external_token {
                out_result.set_external_token(external_token);
            }
            // println!("parent_meta: {:?}", parent_meta);
            if let Some(mut parent_meta) = parent_meta {
                let mut msg = parent_meta
                    .get("message")
                    .unwrap()
                    .expect_string_ref()
                    .unwrap();

                while msg == "success" {
                    // has parent, need to interrupt the parent thread and switch context
                    // and get backtrace and caller meta (if exists) for the parent thread
                    let parent_id = parent_meta.get("id").unwrap().expect_string_ref().unwrap();
                    let parent_session = STATES.session_by_tag(parent_id).unwrap();
                    let (parent_sid, parent_in_custom_ctx) = parent_session
                        .read_with(|session| (session.sid(), session.is_in_custom_context()))
                        .await;
                    inspect_gtid = STATES
                        .global_thread_ids_for_session(parent_sid)
                        .first()
                        .copied()
                        .unwrap();

                    // ------------ [BEGIN] interrupt the parent thread ------------
                    let bt_data = if !parent_in_custom_ctx {
                        debug!("try to swap context for {}", parent_sid);

                        let related_session = if Config::global().handle_migration() {
                            let proclet_id = parent_meta
                                .get("proclet_id")
                                .and_then(|value| value.expect_string_ref().ok())
                                .filter(|proclet_id| !proclet_id.is_empty() && *proclet_id != "0");
                            match proclet_id {
                                Some(proclet_id) => match get_proclet_restore_mgr()
                                    .related_session(parent_sid, &proclet_id.to_string())
                                    .await
                                {
                                    Ok(related) => related,
                                    Err(error) => {
                                        error!(?error, "failed to resolve migration session");
                                        break;
                                    }
                                },
                                None => None,
                            }
                        } else {
                            None
                        };

                        let tx = match transaction::begin_with_related(parent_sid, related_session)
                            .await
                        {
                            Ok(tx) => tx,
                            Err(e) => {
                                error!(
                                    "Failed to start transaction for session {}, break call chain here: {:?}",
                                    parent_sid, e
                                );
                                break;
                            }
                        };

                        // interrupt, switch context, get backtrace
                        let intr_resp =
                            api::command(&format!("-exec-interrupt --session {}", parent_sid))
                                .unwrap()
                                .execute_exclusive(tx.lease())
                                .await;

                        if intr_resp.is_err() {
                            // TODO: maybe auto-retry?
                            error!(
                                "Failed to interrupt session {}, break call chain here.",
                                parent_sid
                            );
                            break;
                        }
                        // ------------ [END] interrupt the parent thread ------------

                        // ------------ [BEGIN] switch the context for the parent thread ------------
                        if let Err(e) = Self::wait_for_all_threads_stopped(tx.session()).await {
                            error!(
                                "Failed waiting for session {} to stop before context switch: {:?}",
                                parent_sid, e
                            );
                            break;
                        }

                        let ctx_switch_args = Self::prepare_ctx_switch_args(
                            &parent_meta
                                .get("caller_ctx")
                                .unwrap()
                                .expect_dict_ref()
                                .unwrap(),
                        );
                        let switch_resp =
                            api::command(&format!("-switch-context-custom {}", ctx_switch_args))
                                .unwrap()
                                .target(Target::Thread(inspect_gtid))
                                .execute_exclusive(tx.lease())
                                .await
                                .unwrap();

                        let switch_resp = switch_resp
                            .get_responses()
                            .first()
                            .unwrap()
                            .get_payload()
                            .unwrap();

                        if switch_resp["message"].expect_string_ref().unwrap() != "success" {
                            error!(
                                "Failed to switch context for session {}, breaks here. The call stack might be corrupted.",
                                parent_sid
                            );
                            break;
                        }

                        let ctx_to_save =
                            Self::extract_ctx_from_payload(&switch_resp, inspect_gtid).unwrap();

                        tx.session()
                            .write_with(|session| {
                                session.set_current_context(Some(ctx_to_save));
                                session.set_in_custom_context(true);
                            })
                            .await;

                        self.handle_migration_if_enabled(inspect_gtid, &parent_meta, Some(&tx))
                            .await;
                        self.get_bt_and_caller_meta_locked(&tx, parent_sid, inspect_gtid)
                            .await
                    } else {
                        self.get_bt_and_caller_meta(inspect_gtid).await
                    };

                    // ------------ [BEGIN] get backtrace for the parent thread ------------
                    parent_meta = match bt_data {
                        Ok(data) => {
                            // move the backtrace to the output payload
                            let frames = match Self::get_stack_owned(data.bt) {
                                Some(frames) => frames,
                                None => {
                                    error!("Failed to get backtrace, break the call chain.");
                                    break;
                                }
                            };
                            let (sid, _) = STATES.local_thread_id(inspect_gtid).unwrap().into();
                            let boundary_frame: gdbmi::raw::Value = HashMap::from([
                                ("line".to_string(), "0".into()),
                                ("level".to_string(), "-1".into()),
                                ("func".to_string(), "<boundary>".into()),
                                ("addr".to_string(), "0xDEADBEEF".into()),
                                ("file".to_string(), "???".into()),
                                ("arch".to_string(), "???".into()),
                                ("session".to_string(), sid.to_string().into()),
                                ("thread".to_string(), inspect_gtid.to_string().into()),
                                ("boundary_frame".to_string(), "1".into()),
                            ])
                            .into();
                            Self::get_stack_ref_mut(&mut out_result)
                                .unwrap()
                                .push(boundary_frame);
                            Self::get_stack_ref_mut(&mut out_result)
                                .unwrap()
                                .extend(frames);

                            if let Some(parent_meta) = data.parent_meta {
                                parent_meta
                            } else {
                                debug!("no parent meta, break the call chain");
                                break;
                            }
                        }
                        Err(e) => {
                            error!("Failed to get backtrace (not suppose to happen), break the call chain: {:?}", e);
                            break;
                        }
                    };
                    // ------------ [END] get backtrace for the parent thread ------------
                    msg = parent_meta
                        .get("message")
                        .unwrap()
                        .expect_string_ref()
                        .unwrap();
                }
            }
            // finally, add a reordered frame levels
            // This ensures the reordered levels are incrementally increased from 0..n
            Self::add_reordered_frame_levels(&mut out_result);
            Ok(CommandOutcome::response(out_result, Presentation::Plain))
        } else {
            bail!("bt-remote requires a thread target")
        }
    }
}

#[derive(Debug)]
pub struct ExecNextHandler;

impl ExecNextHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Handler for ExecNextHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if let Target::Thread(_) = &cmd.target {
            let response = api::parsed(cmd)?.execute().await?;
            Ok(CommandOutcome::silent(response))
        } else {
            bail!("exec-next command should specify a thread id by --thread <gtid>")
        }
    }
}

#[derive(Debug)]
pub struct ExecFinishHandler;

impl ExecFinishHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Handler for ExecFinishHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if let Target::Thread(_) = &cmd.target {
            let response = api::parsed(cmd)?.execute().await?;
            Ok(CommandOutcome::silent(response))
        } else {
            bail!("exec-finish command should specify a thread id by --thread <gtid>")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::Ipv4Addr};

    use super::*;
    use crate::cmd_flow::framework_adapter::GrpcAdapter;

    #[test]
    fn distributed_backtrace_extracts_grpc_parent_metadata() {
        let handler = DistributeBacktraceHandler::new(Arc::new(GrpcAdapter));
        let payload: Dict = HashMap::from([
            ("message".to_string(), Value::from("success")),
            (
                "metadata".to_string(),
                Value::from(Dict::from(HashMap::from([
                    (
                        "caller_meta".to_string(),
                        Value::from(Dict::from(HashMap::from([
                            (
                                "ip".to_string(),
                                Value::from(u32::from(Ipv4Addr::new(127, 0, 0, 1)).to_string()),
                            ),
                            ("pid".to_string(), Value::from("42")),
                            ("tid".to_string(), Value::from("7")),
                            ("proclet_id".to_string(), Value::from("0")),
                        ]))),
                    ),
                    (
                        "caller_ctx".to_string(),
                        Value::from(Dict::from(HashMap::from([
                            ("pc".to_string(), Value::from("4096")),
                            ("sp".to_string(), Value::from("8192")),
                            ("fp".to_string(), Value::from("12288")),
                        ]))),
                    ),
                ]))),
            ),
        ])
        .into();

        let extracted = handler
            .extract_remote_metadata(&payload)
            .expect("grpc metadata extraction should succeed");

        assert_eq!(extracted["message"].expect_string_ref().unwrap(), "success");
        assert_eq!(
            extracted["id"].expect_string_ref().unwrap(),
            "127.0.0.1:-42"
        );
        assert_eq!(extracted["pid"].expect_string_ref().unwrap(), "42");
        assert_eq!(extracted["proclet_id"].expect_string_ref().unwrap(), "0");
        assert_eq!(
            extracted["caller_ctx"].expect_dict_ref().unwrap()["pc"]
                .expect_string_ref()
                .unwrap(),
            "4096"
        );
    }

    #[test]
    fn prepare_ctx_switch_args_serializes_scalar_registers() {
        let regs: Dict = HashMap::from([
            ("pc".to_string(), Value::from("4096")),
            ("sp".to_string(), Value::from("8192")),
            ("fp".to_string(), Value::from("12288")),
        ])
        .into();

        let args = DistributeBacktraceHandler::prepare_ctx_switch_args(&regs);

        assert!(args.contains("pc=4096"));
        assert!(args.contains("sp=8192"));
        assert!(args.contains("fp=12288"));
    }
}

#[derive(Debug)]
pub struct ExecStepHandler;

impl ExecStepHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Handler for ExecStepHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if let Target::Thread(_) = &cmd.target {
            let response = api::parsed(cmd)?.execute().await?;
            Ok(CommandOutcome::silent(response))
        } else {
            bail!("exec-step command should specify a thread id by --thread <gtid>")
        }
    }
}

#[derive(Debug)]
pub struct ExecJumpHandler;

#[async_trait]
impl Handler for ExecJumpHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        // Note: `exec-jump` should only be used when session is specified at the moment.
        // otherwise it will be ambiguous which process to jump to.
        // let (target, cmd) = cmd.to_command(PlainFormatter);
        match cmd.target {
            Target::Session(_) => {
                let response = api::parsed(cmd)?.execute().await?;
                Ok(CommandOutcome::response(response, Presentation::Plain))
            }
            _ => bail!("exec-jump command should specify a session"),
        }
    }
}

#[derive(Debug)]
pub struct SendSignalHandler;

#[async_trait]
impl Handler for SendSignalHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if cmd.args.trim().is_empty() {
            bail!("-send-signal command requires a signal argument");
        }

        match cmd.target {
            Target::Session(sid) => {
                // TODO: use signal mgr to check if the signal is valid or not.

                // Signal can only be delivered when the process is stopped in gdb.
                // So first send an interrupt to stop the process.
                let backend = get_debugger_backend();
                api::command(&backend.interrupt_command())?
                    .target(Target::Session(sid))
                    .execute()
                    .await?;

                let signal_cmd =
                    backend.console_exec_command(&format!("signal {}", cmd.args.trim()));
                let mut response = api::command(&signal_cmd)?
                    .target(Target::Session(sid))
                    .execute()
                    .await?;
                if let Some(token) = cmd.external_token {
                    response.set_external_token(token);
                }
                debug!("-send-signal command completed: {}", signal_cmd);
                Ok(CommandOutcome::silent(response))
            }
            _ => bail!("-send-signal command should specify a session"),
        }
    }
}

#[derive(Debug)]
pub struct ListSignalsHandler;

#[async_trait]
impl Handler for ListSignalsHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) -> Result<CommandOutcome> {
        if !cmd.args.trim().is_empty() {
            bail!("-list-signals command needs no argument. raw: {}", cmd.args);
        }

        match cmd.target {
            Target::Session(sid) => {
                // TODO: use signal mgr to cache results?
                // so that we can directly return if it is cached already.
                let list_signal_cmd = format!(
                    "{}-list-signals",
                    cmd.external_token
                        .map(|token| token.to_string())
                        .unwrap_or("".to_string())
                );
                let response = api::command(&list_signal_cmd)?
                    .target(Target::Session(sid))
                    .execute()
                    .await?;
                debug!("-list-signals command completed: {}", list_signal_cmd);
                Ok(CommandOutcome::response(response, Presentation::Plain))
            }
            _ => bail!("-list-signals command should specify a session"),
        }
    }
}
