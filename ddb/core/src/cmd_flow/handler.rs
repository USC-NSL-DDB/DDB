use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{bail, Result};
use async_trait::async_trait;
use gdbmi::raw::{Dict, Value};
use tokio::{
    sync::{RwLock, RwLockWriteGuard},
    task::JoinHandle,
};
use tracing::{debug, error, warn};

use crate::{
    common::Config,
    feature::get_proclet_restore_mgr,
    state::{
        get_bkpt_mgr, BkptMeta, LocalThreadId, SessionMeta, ThreadContext, ThreadStatus, STATES,
    },
};

use super::{
    api, emit_static, framework_adapter::FrameworkCommandAdapter, input::ParsedInputCmd, output,
    router::Target, FinishedCmd, GdbDataErr, NullFormatter, PlainFormatter,
    ProcessReadableFormatter, ThreadInfoFormatter,
};

/// Handler trait for processing parsed commands with routing and formatting logic
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
/// 3. **Error Handling**: Handlers should log errors appropriately but may choose to
///    emit error responses rather than propagating errors upward
///
/// 4. **Formatter Selection**: Handlers choose appropriate formatters based on command
///    type and expected output format (e.g., `ThreadInfoFormatter` for thread info)
///
/// # Example Implementation
///
/// ```no_run
/// # use async_trait::async_trait;
/// # use super::{Handler, ParsedInputCmd, Router, PlainFormatter};
/// # use std::sync::Arc;
/// struct MyHandler {}
///
/// #[async_trait]
/// impl Handler for MyHandler {
///     async fn process_cmd(&self, cmd: ParsedInputCmd) {
///         // Convert parsed command to executable command with formatter
///         let (target, cmd) = cmd.to_command(PlainFormatter);
///         
///         // Route via router (respects target semantics)
///         self.router.send_to(target, cmd);
///     }
/// }
/// ```
#[async_trait]
pub trait Handler: Send + Sync {
    async fn process_cmd(&self, cmd: ParsedInputCmd);
}

pub struct DefaultHandler;

impl DefaultHandler {
    pub fn new() -> Self {
        DefaultHandler
    }
}

#[async_trait]
impl Handler for DefaultHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let _ = cmd.send().with(PlainFormatter).to_default_target();
    }
}

pub struct BreakInsertHandler {
    base: DefaultHandler,
}

impl BreakInsertHandler {
    pub fn new() -> Self {
        BreakInsertHandler {
            base: DefaultHandler::new(),
        }
    }
}

#[async_trait]
impl Handler for BreakInsertHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let full_cmd = cmd.full_cmd();
        let results = cmd.send_and_return().to_default_target().await;
        if let Ok(results) = results {
            for resp in results.get_responses() {
                if resp.get_message() == "done" {
                    get_bkpt_mgr().add_by_sid(resp.get_sid(), BkptMeta::new(full_cmd.clone()));
                } else {
                    warn!("Failed to insert breakpoint from dbg: {:?}", resp);
                }
            }
            emit_static(results, PlainFormatter);
        } else {
            error!("Failed to insert breakpoint: {:?}", results);
        }
    }
}

pub struct ThreadInfoHandler;

impl ThreadInfoHandler {
    pub fn new() -> Self {
        ThreadInfoHandler
    }
}

#[async_trait]
impl Handler for ThreadInfoHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let _ = cmd.send().with(ThreadInfoFormatter).to(Target::Broadcast);
    }
}

pub struct ContinueHandler;

impl ContinueHandler {
    pub fn new() -> Self {
        ContinueHandler
    }
}

impl ContinueHandler {
    async fn switch_context_and_cont(
        cont_cmd: ParsedInputCmd,
        mut session: RwLockWriteGuard<'_, SessionMeta>,
    ) -> Result<()> {
        if let Some(ctx) = &session.curr_ctx {
            let target = Target::Thread(ctx.tid);
            let ctx = Self::prepare_ctx_switch_args(&ctx);
            let r = api::send_and_return(&format!("-switch-context-custom {}", ctx))
                .unwrap()
                .to(target)
                .await?;
            let responses = r.get_responses();
            let sid = session.sid;
            if responses.len() != 1
                || responses[0].get_payload().unwrap()["message"]
                    .expect_string_ref()
                    .unwrap()
                    != "success"
            {
                // Fail to restore the context, skip continue
                // TODO: maybe auto-retry is desired?
                session.in_custom_ctx = true;
                drop(session);
                bail!("Failed to restore context for session {}", sid);
            } else {
                // Context restored, continue
                session.in_custom_ctx = false;
                // early drop to release the lock, we don't need it to lock the session anymore
                // for waiting for the continue response.
                drop(session);
                Self::cont(Target::Session(sid), cont_cmd);
                return Ok(());
            }
        }
        // TODO: handle error if no context is found
        Ok(())
    }

    #[inline]
    fn cont(target: Target, cont_cmd: ParsedInputCmd) {
        let _ = cont_cmd.send().with(PlainFormatter).to(target);
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let ss = STATES.get_all_sessions();

        // reset all proclet cache and clean up restored proclet heap.
        get_proclet_restore_mgr().reset().await;

        let tasks = match &cmd.target {
            Target::Session(sid) => {
                // Note: need to first check if the session is in custom context.
                // If so, switch context and continue.
                let tasks: Vec<_> = ss
                    .iter()
                    .map(|s| -> JoinHandle<Result<()>> {
                        let s = s.clone();
                        let sid = sid.clone();
                        let cmd = cmd.clone();
                        tokio::spawn(async move {
                            let s = s.write().await;
                            if s.sid == sid {
                                if s.in_custom_ctx {
                                    // need to restore context before continue
                                    Self::switch_context_and_cont(cmd, s).await?
                                } else {
                                    // no need to restore context, just continue
                                    Self::cont(Target::Session(sid), cmd);
                                }
                            }
                            Ok(())
                        })
                    })
                    .collect();
                tasks
            }
            // no session specified, should revert back to broadcast to all sessions.
            _ => {
                // Note: need to first check if the session is in custom context.
                // If so, switch context and continue.
                let tasks: Vec<_> = ss
                    .iter()
                    .map(|s| {
                        let s = s.clone();
                        let cmd = cmd.clone();
                        tokio::spawn(async move {
                            let s = s.write().await;
                            if s.in_custom_ctx {
                                // need to restore context before continue
                                Self::switch_context_and_cont(cmd, s).await?
                            } else {
                                // no need to restore context, just continue
                                Self::cont(Target::Session(s.sid), cmd);
                            }
                            Ok(())
                        })
                    })
                    .collect();
                tasks
            }
        };

        for result in futures::future::join_all(tasks).await {
            if let Err(e) = result {
                error!("Failed to continue: {:?}", e);
            }
        }
    }
}

pub struct InterruptHandler;

impl InterruptHandler {
    pub fn new() -> Self {
        InterruptHandler
    }
}

#[async_trait]
impl Handler for InterruptHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        match cmd.target {
            Target::Session(sid) => {
                let ss = STATES.get_session(sid);
                if ss.is_some() {
                    // Note: send interrupt to running process. Ignore thread granularity.
                    // skips checking if the thread is running or not.
                    let _ = cmd.send().with(PlainFormatter).to_default_target();
                }
            }
            _ => {
                // broadcast to all sessions
                let _ = cmd.send().with(PlainFormatter).to(Target::Broadcast);
            }
        }
    }
}

pub struct ListHandler;

impl ListHandler {
    pub fn new() -> Self {
        ListHandler
    }
}

#[async_trait]
impl Handler for ListHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        // FIXME: a naive implementation here, just select the first session
        // This command is need for CLI (to list out sources), but probably not for GUI?
        STATES.set_curr_session(1);
        let _ = cmd.send().with(PlainFormatter).to(Target::CurrSession);
    }
}

pub struct ThreadSelectHandler;

impl ThreadSelectHandler {
    pub fn new() -> Self {
        ThreadSelectHandler
    }
}

#[async_trait]
impl Handler for ThreadSelectHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let parts = cmd.args.split_whitespace().collect::<Vec<_>>();
        if !parts.is_empty() {
            let gtid = parts.last().unwrap().parse::<u64>().unwrap();
            let (sid, tid) = STATES.get_ltid_by_gtid(gtid).unwrap().into();
            let target = Target::Session(sid);
            let _ = api::send(&format!("-thread-select {}", tid))
                .unwrap()
                .with(PlainFormatter)
                .to(target);
        } else {
            let _ = cmd.send().with(PlainFormatter).to_default_target();
        }
    }
}

pub struct ListGroupsHandler;

impl ListGroupsHandler {
    pub fn new() -> Self {
        ListGroupsHandler
    }
}

#[async_trait]
impl Handler for ListGroupsHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let _ = cmd
            .send()
            .with(ProcessReadableFormatter)
            .to(Target::Broadcast);
    }
}

struct BacktraceData {
    bt: FinishedCmd,
    parent_meta: Option<Dict>,
}

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
            .ok_or(GdbDataErr::MissingEntry("metadata".into()))?;

        let msg = payload
            .get("message")
            .ok_or(GdbDataErr::MissingEntry("message".into()))?
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
            .ok_or(GdbDataErr::MissingEntry("old_ctx".into()))?
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

    async fn check_thread_status(
        s: &Arc<RwLock<SessionMeta>>,
    ) -> Result<RwLockWriteGuard<'_, SessionMeta>> {
        // set deadline to 1s
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        // busy wait for the interrupt to take effect for sure
        // e.g. the thread status is changed to STOPPED
        loop {
            let write_guard = s.write().await;
            debug!("check thread status for {}", write_guard.tag);
            if write_guard
                .t_status
                .iter()
                .filter(|(_, v)| **v != ThreadStatus::STOPPED)
                .count()
                != 0
            {
                if std::time::Instant::now() > deadline {
                    bail!("wait too long for interrupt to take effect, break call chain here.");
                }
                // Some threads are still considered as running
                // drop the write guard, yield, and retry later.
                drop(write_guard);
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            } else {
                // All threads are stopped, break the loop
                // return the write guard
                return Ok(write_guard);
            }
        }
    }
}

impl DistributeBacktraceHandler {
    async fn get_bt_and_caller_meta(&self, gtid: u64) -> Result<BacktraceData> {
        // ------------ [BEGIN] get backtrace for the current thread ------------
        // `ParsedInputCmd` already swapped the gtid with local tid.
        let mut stack_resp = api::send_and_return(&format!("-stack-list-frames --thread {}", gtid))
            .unwrap()
            .to_default_target()
            .await?;

        let (sid, _) = STATES.get_ltid_by_gtid(gtid).unwrap().into();
        for frame in Self::get_stack_ref_mut(&mut stack_resp).iter_mut() {
            let frame = frame.expect_dict_ref_mut().unwrap();
            frame.insert("session".to_string(), sid.to_string().into());
            frame.insert("thread".to_string(), gtid.to_string().into());
        }
        // ------------ [END] get backtrace for the current thread ------------

        let dbt_cmd = self.adapter.get_bt_command_name();

        // ------------ [BEGIN] get caller metadata for the current threads ------------
        let resp = api::send_and_return(&dbt_cmd)
            .unwrap()
            .to(Target::Thread(gtid))
            .await
            .unwrap(); // TODO: better error handling.

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

    async fn handle_migration_if_enabled(&self, inspect_gtid: u64, parent_meta: &Dict) {
        if Config::global().handle_migration() {
            if let Some(LocalThreadId(sid, _)) = STATES.get_ltid_by_gtid(inspect_gtid) {
                let proclet_id = parent_meta
                    .get("proclet_id")
                    .unwrap()
                    .expect_string_ref()
                    .unwrap()
                    .to_string();
                match get_proclet_restore_mgr()
                    .handle_proclet_restoration(sid, &proclet_id)
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

    fn get_stack_ref_mut<'a>(response: &'a mut FinishedCmd) -> &'a mut Vec<Value> {
        response
            .get_responses_mut()
            .first_mut()
            .unwrap()
            .get_payload_mut()
            .unwrap()
            .get_mut("stack")
            .unwrap()
            .expect_list_ref_mut()
            .unwrap()
    }

    fn get_stack_owned(mut response: FinishedCmd) -> Vec<Value> {
        response
            .get_responses_mut()
            .first_mut()
            .unwrap()
            .get_payload_mut()
            .unwrap()
            .remove("stack")
            .unwrap()
            .expect_list()
            .unwrap()
    }
    fn update_frame_levels<'a>(responses: &'a mut FinishedCmd) {
        let stack = Self::get_stack_ref_mut(responses);
        for (i, frame) in stack.iter_mut().enumerate() {
            let frame = frame.expect_dict_ref_mut().unwrap();
            frame.insert("level".to_string(), (i as u64).to_string().into());
        }
    }
}

#[async_trait]
impl Handler for DistributeBacktraceHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        // let all_sessions = STATES.get_all_sessions();
        // for session in all_sessions {
        //     let session_guard = session.read().await;
        //     println!("session tag: {}", session_guard.tag);
        // }
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
                    error!("Failed to get backtrace (not suppose to happen), break the call chain: {:?}", e);
                    return;
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
                    let parent_s = STATES.get_session_by_tag(parent_id).await.unwrap();
                    let parent_s_guard = parent_s.read().await;
                    let parent_sid = parent_s_guard.sid;
                    let parent_in_custom_ctx = parent_s_guard.in_custom_ctx;
                    drop(parent_s_guard);
                    inspect_gtid = STATES.get_gtids_by_sid(parent_sid).first().unwrap().clone();

                    // ------------ [BEGIN] interrupt the parent thread ------------
                    if !parent_in_custom_ctx {
                        debug!("try to swap context for {}", parent_sid);
                        // interrupt, switch context, get backtrace
                        let intr_resp = api::send_and_return(&format!(
                            "-exec-interrupt --session {}",
                            parent_sid
                        ))
                        .unwrap()
                        .to_default_target()
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
                        let mut w_guard = Self::check_thread_status(&parent_s).await.unwrap();

                        // start to switch context, hold the write lock to create critical section.
                        // to this point, all threads in this sessions are considered as stopped.
                        let ctx_switch_args = Self::prepare_ctx_switch_args(
                            &parent_meta
                                .get("caller_ctx")
                                .unwrap()
                                .expect_dict_ref()
                                .unwrap(),
                        );
                        let switch_resp = api::send_and_return(&format!(
                            "-switch-context-custom {}",
                            ctx_switch_args
                        ))
                        .unwrap()
                        .to(Target::Thread(inspect_gtid))
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
                        }

                        let ctx_to_save =
                            Self::extract_ctx_from_payload(&switch_resp, inspect_gtid).unwrap();

                        w_guard.curr_ctx = Some(ctx_to_save);
                        w_guard.in_custom_ctx = true;

                        self.handle_migration_if_enabled(inspect_gtid, &parent_meta)
                            .await;
                    }
                    // ------------ [BEGIN] get backtrace for the parent thread ------------
                    let bt_data = self.get_bt_and_caller_meta(inspect_gtid).await;
                    // drop the write guard at this point to hold the critical section when getting the backtrace
                    // drop(w_guard);
                    parent_meta = match bt_data {
                        Ok(data) => {
                            // move the backtrace to the output payload
                            let frames = Self::get_stack_owned(data.bt);
                            let (sid, _) = STATES.get_ltid_by_gtid(inspect_gtid).unwrap().into();
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
                            Self::get_stack_ref_mut(&mut out_result).push(boundary_frame);
                            Self::get_stack_ref_mut(&mut out_result).extend(frames);

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
            // finally, update frame levels
            // This ensures the levels are incrementally increased from 0..n
            Self::update_frame_levels(&mut out_result);
            output::emit_static(out_result, PlainFormatter);
        }
    }
}

pub struct ExecNextHandler;

impl ExecNextHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Handler for ExecNextHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        // let parts = cmd.args.split_whitespace().collect::<Vec<_>>();
        // if !parts.is_empty() {
        //     let gtid = parts.last().unwrap().parse::<u64>().unwrap();
        //     // let (sid, tid) = STATES.get_ltid_by_gtid(gtid).unwrap().into();
        //     // let target = Target::Session(sid);
        //     let target = Target::Thread(gtid);
        //     self.router.send_to(target, cmd.to_command(PlainFormatter).1);
        // } else {
        //     warn!("exec-next command should specify a thread id");
        // }
        //
        //
        if let Target::Thread(_) = &cmd.target {
            let _ = cmd.send().with(NullFormatter).to_default_target();
        } else {
            error!("exec-next command should specify a thread id by --thread <gtid>");
        }
    }
}

pub struct ExecFinishHandler;

impl ExecFinishHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Handler for ExecFinishHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if let Target::Thread(_) = &cmd.target {
            let _ = cmd.send().with(NullFormatter).to_default_target();
        } else {
            error!("exec-finish command should specify a thread id by --thread <gtid>");
        }
    }
}

pub struct ExecStepHandler;

impl ExecStepHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Handler for ExecStepHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if let Target::Thread(_) = &cmd.target {
            let _ = cmd.send().with(NullFormatter).to_default_target();
        } else {
            error!("exec-step command should specify a thread id by --thread <gtid>");
        }
    }
}

pub struct ExecJumpHandler;

#[async_trait]
impl Handler for ExecJumpHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        // Note: `exec-jump` should only be used when session is specified at the moment.
        // otherwise it will be ambiguous which process to jump to.
        // let (target, cmd) = cmd.to_command(PlainFormatter);
        match cmd.target {
            Target::Session(_) => {
                let _ = cmd.send().with(PlainFormatter).to_default_target();
            }
            _ => {
                error!("exec-jump command should specify a session");
            }
        }
    }
}
