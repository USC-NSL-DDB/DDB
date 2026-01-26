use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use gdbmi::raw::{Dict, Value};
use tokio::{sync::RwLockWriteGuard, task::JoinHandle};
use tracing::{debug, error, warn};

use crate::{
    cmd_flow::{emit_error, transaction},
    common::Config,
    dbg_cmd::{DbgCmdGenerator, GdbCmd},
    dbg_parser::gdb_parser::{bkpt_deleted_payload, MIFormatter},
    feature::get_proclet_restore_mgr,
    state::{
        get_bkpt_mgr, get_group_mgr, get_signal_mgr, get_state_mgr, BkptLoc, GroupSubBkpt,
        LocalThreadId, SessionMeta, SessionRef, SessionSubBkpt, SubBkptType, ThreadContext,
        ThreadStatus, STATES,
    },
};

use super::{
    api, framework_adapter::FrameworkCommandAdapter, input::ParsedInputCmd, output, router::Target,
    FinishedCmd, GdbDataErr, NullFormatter, PlainFormatter, ProcessReadableFormatter,
    ThreadInfoFormatter,
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

pub struct BreakInsertHandler;
impl BreakInsertHandler {
    pub fn new() -> Self {
        BreakInsertHandler
    }
}

impl BreakInsertHandler {
    async fn insert_bkpts_for_group(major_bkpt_id: u64, cmd: &str, gid: u64) -> Result<()> {
        let grp_bkpt = GroupSubBkpt::new(gid);

        // Check if group exists and has active sessions
        let grp = match get_group_mgr().get_grp_by_id(gid) {
            Some(g) => g,
            None => {
                warn!("Group {} does not exist", gid);
                return Err(anyhow!("Group {} does not exist", gid));
            }
        };
        let sids = grp.get_sids();

        // Only send breakpoint command if group has active sessions
        // If empty, the breakpoint will be applied later when sessions join via sync_bkpts_state()
        if !sids.is_empty() {
            let ret = api::send_and_return(cmd)
                .unwrap()
                .to(Target::Group(gid))
                .await?;
            for resp in ret.get_responses() {
                let bkpt_info = resp
                    .get_payload()
                    .unwrap()
                    .get("bkpt")
                    .unwrap()
                    .expect_dict_ref()
                    .unwrap();
                let local_bkpt_id = bkpt_info
                    .get("number")
                    .unwrap()
                    .expect_string_ref()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                let times = bkpt_info
                    .get("times")
                    .unwrap()
                    .expect_string_ref()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                // TODO: work out how to store `times` information.
                grp_bkpt.add_local_bkpt(resp.get_sid(), local_bkpt_id);
            }
        }

        let subbkpt = SubBkptType::Group(grp_bkpt);
        get_bkpt_mgr().add_subbkpt(major_bkpt_id, subbkpt);
        Ok(())
    }

    async fn insert_bkpts_for_session(major_bkpt_id: u64, cmd: &str, sid: u64) -> Result<()> {
        let ret = api::send_and_return(cmd)
            .unwrap()
            .to(Target::Session(sid))
            .await?;
        let bkpt_info = ret
            .get_responses()
            .first()
            .unwrap()
            .get_payload()
            .unwrap()
            .get("bkpt")
            .unwrap()
            .expect_dict_ref()
            .unwrap();
        let local_bkpt_id = bkpt_info
            .get("number")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let times = bkpt_info
            .get("times")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        // TODO: work out how to store `times` information.
        let subbkpt = SubBkptType::Session(SessionSubBkpt::new(local_bkpt_id, sid));
        get_bkpt_mgr().add_subbkpt(major_bkpt_id, subbkpt);
        Ok(())
    }
}

#[async_trait]
impl Handler for BreakInsertHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if !matches!(
            cmd.target,
            Target::Session(_) | Target::Group(_) | Target::Multiple(_)
        ) {
            warn!(
                "Unsupported target for BreakInsertHandler: {:?}",
                cmd.target
            );
            return;
        }

        let full_cmd = cmd.full_cmd();
        let args = &cmd.args;
        // Assumption: the args contains the breakpoint loc at the last place.
        // In the last element, the first element is the file path and the second element is the line number.
        let _bkpt_loc: [&str; 2]; // Use stack allocation for perf optimization.
        let bkpt_loc_parts = args
            .trim()
            .rsplit_once(char::is_whitespace)
            .unwrap_or(("", args))
            .1 // the result is like "file_path:line_no", including the quotes.
            .trim_matches(['"', '\''])
            .split_once(":")
            .unwrap();
        _bkpt_loc = [bkpt_loc_parts.0, bkpt_loc_parts.1];
        let bkpt_loc: BkptLoc = _bkpt_loc.into();
        let bkpt_id = get_bkpt_mgr().add_bkpt(bkpt_loc);

        match cmd.target {
            Target::Session(sid) => {
                if let Err(e) = Self::insert_bkpts_for_session(bkpt_id, &full_cmd, sid).await {
                    let err_msg = format!(
                        "Failed to insert breakpoint into session {}: {}",
                        sid,
                        e.to_string()
                    );
                    emit_error(&err_msg, cmd.external_token);
                    return;
                }
            }
            Target::Group(gid) => {
                if let Err(e) = Self::insert_bkpts_for_group(bkpt_id, &full_cmd, gid).await {
                    let err_msg = format!(
                        "Failed to insert breakpoint into group {}: {}",
                        gid,
                        e.to_string()
                    );
                    emit_error(&err_msg, cmd.external_token);
                    return;
                }
            }
            Target::Multiple(targets) => {
                // dedup to ensure:
                // 1. no deuplicate targets
                // 2. if the session targets are already included in one of the group targets, skip them.
                // 3. drop other targets, only support for session and group targets.
                // result: deduped Vec<Target>
                // let mut deduped_targets: Vec<Target> = Vec::new();
                let groupped_sids = targets
                    .iter()
                    .filter_map(|ele| match ele {
                        Target::Group(gid) => get_group_mgr()
                            .get_grp_by_id(*gid)
                            .map(|grp| grp.get_sids().clone()),
                        _ => None,
                    })
                    .flatten()
                    .collect::<HashSet<u64>>();
                let dedupped_targets = targets
                    .iter()
                    .filter(|target| match target {
                        Target::Session(sid) => !groupped_sids.contains(sid),
                        _ => true,
                    })
                    .collect::<Vec<&Target>>();
                for t in dedupped_targets {
                    match *t {
                        Target::Session(sid) => {
                            if let Err(e) =
                                Self::insert_bkpts_for_session(bkpt_id, &full_cmd, sid).await
                            {
                                warn!(
                                    "Failed to insert breakpoint into session {}: {}",
                                    sid,
                                    e.to_string()
                                );
                            }
                        }
                        Target::Group(gid) => {
                            if let Err(e) =
                                Self::insert_bkpts_for_group(bkpt_id, &full_cmd, gid).await
                            {
                                warn!(
                                    "Failed to insert breakpoint into group {}: {}",
                                    gid,
                                    e.to_string()
                                );
                            }
                        }
                        _ => {
                            // skip other target types
                        }
                    }
                }
            }
            _ => {
                warn!(
                    "Unsupported target for BreakInsertHandler: {:?}",
                    cmd.target
                );
                return;
            }
        }

        match get_bkpt_mgr().get_bkpt_by_id(bkpt_id) {
            Some(bkpt) => {
                let out = MIFormatter::format("^", "done", Some(&bkpt.into()), cmd.external_token);
                println!("{}", out);
                debug!("output: {}", out);
            }
            None => {
                warn!("Failed to find inserted breakpoint with id {}", bkpt_id);
            }
        }
    }
}

pub struct BreakDeleteHandler;
impl BreakDeleteHandler {
    pub fn new() -> Self {
        BreakDeleteHandler
    }
}

impl BreakDeleteHandler {
    async fn delete_local_bkpt(sid: u64, local_bkpt_id: u64) -> Result<()> {
        let ret = api::send_and_return(&format!("-break-delete {}", local_bkpt_id))
            .unwrap()
            .to(Target::Session(sid))
            .await?;
        let response = ret.get_responses().first().unwrap();
        if response.get_message() == "done" {
            get_bkpt_mgr().remove_local_bkpt_id_index(sid, local_bkpt_id);
            Ok(())
        } else {
            warn!(
                "Failed to delete local breakpoint {} from session {}: {:?}",
                local_bkpt_id, sid, response
            );
            bail!(
                "Failed to delete local breakpoint {} from session {}",
                local_bkpt_id,
                sid
            );
        }
    }

    async fn delete_subbkpt(bkpt_id: u64, subbkpt_id: u64) -> Result<()> {
        if let Some(subbkpt) = get_bkpt_mgr().get_subbkpt(bkpt_id, subbkpt_id) {
            match subbkpt.get_type() {
                SubBkptType::Session(sess_subbkpt) => {
                    let sid = sess_subbkpt.get_target_session();
                    let local_bkpt_id = sess_subbkpt.get_local_bkpt_id();
                    let ret = Self::delete_local_bkpt(sid, local_bkpt_id).await;
                    match ret {
                        Ok(_) => {
                            get_bkpt_mgr().delete_subbkpt(bkpt_id, subbkpt_id);
                            return Ok(());
                        }
                        Err(e) => {
                            bail!(
                                "Failed to delete breakpoint {} from session {}. Error: {}",
                                local_bkpt_id,
                                sid,
                                e
                            );
                        }
                    }
                }
                SubBkptType::Group(group_subbkpt) => {
                    let mut error = false;
                    for (sid, local_bkpt_id) in group_subbkpt.get_local_ids() {
                        let ret = Self::delete_local_bkpt(sid, local_bkpt_id).await;
                        match ret {
                            Ok(_) => {}
                            Err(e) => {
                                error = true;
                                error!(
                                    "Failed to delete breakpoint {} from session {}: {}",
                                    local_bkpt_id, sid, e
                                );
                            }
                        }
                    }
                    if error {
                        bail!(
                            "Failed to delete some breakpoints from group sub-breakpoint {}",
                            subbkpt_id
                        );
                    } else {
                        get_bkpt_mgr().delete_subbkpt(bkpt_id, subbkpt_id);
                        return Ok(());
                    }
                }
            }
        } else {
            bail!(
                "No sub-breakpoint found for deletion with bkpt_id {} and subbkpt_id {}",
                bkpt_id,
                subbkpt_id
            );
        }
    }
}

#[async_trait]
impl Handler for BreakDeleteHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let args = cmd.args.trim();
        if args.is_empty() {
            warn!("No breakpoint id provided for deletion.");
            return;
        }

        let bkpt_id: u64;
        if let Some((bkpt_id_str, subbkpt_id_str)) = args.split_once(char::is_whitespace) {
            let mut modified = false;
            bkpt_id = match bkpt_id_str.parse::<u64>() {
                Ok(id) => id,
                Err(e) => {
                    emit_error(
                        &format!("Invalid breakpoint id {}: {:?}", bkpt_id_str, e),
                        cmd.external_token,
                    );
                    return;
                }
            };
            let subbkpt_id = match subbkpt_id_str.parse::<u64>() {
                Ok(id) => id,
                Err(e) => {
                    emit_error(
                        &format!("Invalid sub-breakpoint id {}: {:?}", subbkpt_id_str, e),
                        cmd.external_token,
                    );
                    return;
                }
            };
            match Self::delete_subbkpt(bkpt_id, subbkpt_id).await {
                Ok(_) => {
                    modified = true;
                }
                Err(e) => {
                    warn!(
                        "Failed to delete sub-breakpoint {} of breakpoint {}: {:?}",
                        subbkpt_id, bkpt_id, e
                    );
                }
            }
            if modified {
                if let Some(empty) = get_bkpt_mgr().is_bkpt_empty(bkpt_id) {
                    if !empty {
                        // still has other sub-breakpoints, emit breakpoint-modified event
                        if let Some(bkpt) = get_bkpt_mgr().get_bkpt_by_id(bkpt_id) {
                            let out = MIFormatter::format("^", "done", None, cmd.external_token);
                            println!("{}", out);
                            debug!("output: {}", out);

                            let out = MIFormatter::format(
                                "=",
                                "breakpoint-modified",
                                Some(&bkpt.into()),
                                None,
                            );
                            println!("{}", out);
                            debug!("output: {}", out);
                        }
                    } else {
                        get_bkpt_mgr().delete_bkpt(bkpt_id);
                        let out = MIFormatter::format("^", "done", None, cmd.external_token);
                        println!("{}", out);
                        debug!("output: {}", out);
                        let out = MIFormatter::format(
                            "=",
                            "breakpoint-deleted",
                            Some(&bkpt_deleted_payload(bkpt_id)),
                            None,
                        );
                        println!("{}", out);
                        debug!("output: {}", out);
                    }
                }
            }
        } else {
            bkpt_id = match args.parse::<u64>() {
                Ok(id) => id,
                Err(e) => {
                    emit_error(
                        &format!("Invalid breakpoint id {}: {:?}", args, e),
                        cmd.external_token,
                    );
                    return;
                }
            };
            for (sid, local_bkpt_id) in get_bkpt_mgr().get_local_bkpt_ids(bkpt_id) {
                match Self::delete_local_bkpt(sid, local_bkpt_id).await {
                    Ok(_) => {}
                    Err(e) => {
                        warn!(
                            "Failed to delete local breakpoint {} from session {}: {}",
                            local_bkpt_id,
                            sid,
                            e.to_string()
                        );
                    }
                }
            }

            get_bkpt_mgr().delete_bkpt(bkpt_id);
            let out = MIFormatter::format(
                "=",
                "breakpoint-deleted",
                Some(&bkpt_deleted_payload(bkpt_id)),
                None,
            );
            println!("{}", out);
            debug!("output: {}", out);

            let out = MIFormatter::format("^", "done", None, cmd.external_token);
            println!("{}", out);
            debug!("output: {}", out);
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
        match cmd.target {
            Target::Thread(tid) => {
                // args can only be "--thread <local_tid>" in this case.
                let (_, ltid) = cmd.args.split_once(char::is_whitespace).unwrap();
                let thrd_info_cmd = format!(
                    "{}-thread-info {}",
                    cmd.external_token
                        .map(|token| { token.to_string() })
                        .unwrap_or("".to_string()),
                    ltid
                );
                let _ = api::send(&thrd_info_cmd)
                    .unwrap()
                    .with(ThreadInfoFormatter)
                    .to(Target::Thread(tid));
            }
            _ => {
                let _ = cmd.send().with(ThreadInfoFormatter).to_default_target();
            }
        }
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
                bail!("Failed to restore context for session {}", sid);
            } else {
                // Context restored, continue
                session.in_custom_ctx = false;
                Self::cont(Target::Session(sid), cont_cmd, session);
                return Ok(());
            }
        }
        // TODO: handle error if no context is found
        Ok(())
    }

    #[inline]
    fn cont(
        target: Target,
        cont_cmd: ParsedInputCmd,
        mut session: RwLockWriteGuard<'_, SessionMeta>,
    ) {
        let _ = cont_cmd.send().with(PlainFormatter).to(target);
        // NOTE: assume all-stop mode here.
        session.update_all_status(ThreadStatus::RUNNING);
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
                            let s = s.meta.write().await;
                            if s.sid == sid {
                                if s.in_custom_ctx {
                                    // need to restore context before continue
                                    Self::switch_context_and_cont(cmd, s).await?
                                } else {
                                    // no need to restore context, just continue
                                    Self::cont(Target::Session(sid), cmd, s);
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
                            let s = s.meta.write().await;
                            if s.in_custom_ctx {
                                // need to restore context before continue
                                Self::switch_context_and_cont(cmd, s).await?
                            } else {
                                // no need to restore context, just continue
                                Self::cont(Target::Session(s.sid), cmd, s);
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

    async fn check_thread_status(s: &SessionRef) -> Result<RwLockWriteGuard<'_, SessionMeta>> {
        // set deadline to 1s
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        // busy wait for the interrupt to take effect for sure
        // e.g. the thread status is changed to STOPPED
        loop {
            let write_guard = s.meta.write().await;
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
        let (sid, _) = get_state_mgr().get_ltid_by_gtid(gtid).unwrap().into();

        // Acquire transaction lock for exclusive command sequence access
        let tx = transaction::begin(sid)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Send interrupt command
        // let _ = api::send_and_return(&"-exec-interrupt")
        //     .unwrap()
        //     .to(Target::Thread(gtid))
        //     .await?;

        // Update thread status with short-lived lock
        // {
        //     let mut write_guard = tx.session().meta.write().await;
        //     write_guard.update_all_status(ThreadStatus::STOPPED);
        // }

        // Get stack frames
        let mut stack_resp = api::send_and_return(&format!("-stack-list-frames --thread {}", gtid))
            .unwrap()
            .to_default_target()
            .await?;

        let stack = Self::get_stack_ref_mut(&mut stack_resp)
            .ok_or(anyhow!("Unable to get stack frames from response"))?;
        for frame in stack.iter_mut() {
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

        // Transaction lock (tx) is released here when it goes out of scope

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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
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
                    emit_error(&err_msg, cmd.external_token);
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
                    let parent_s_guard = parent_s.meta.read().await;
                    let parent_sid = parent_s_guard.sid;
                    let parent_in_custom_ctx = parent_s_guard.in_custom_ctx;
                    drop(parent_s_guard);
                    inspect_gtid = STATES.get_gtids_by_sid(parent_sid).first().unwrap().clone();

                    // ------------ [BEGIN] interrupt the parent thread ------------
                    if !parent_in_custom_ctx {
                        debug!("try to swap context for {}", parent_sid);

                        // TODO: Refactor this section to use transaction::begin(parent_sid)
                        // instead of check_thread_status returning a write guard held across .await points.
                        // The current pattern holds RwLock across multiple awaits which can cause issues.
                        // See get_bt_and_caller_meta for the proper pattern using SessionTransaction.

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
                            let frames = match Self::get_stack_owned(data.bt) {
                                Some(frames) => frames,
                                None => {
                                    error!("Failed to get backtrace, break the call chain.");
                                    break;
                                }
                            };
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

pub struct SendSignalHandler;

#[async_trait]
impl Handler for SendSignalHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if cmd.args.trim().is_empty() {
            error!("-send-signal command requires a signal argument");
            return;
        }

        match cmd.target {
            Target::Session(sid) => {
                // TODO: use signal mgr to check if the signal is valid or not.

                // Signal can only be delivered when the process is stopped in gdb.
                // So first send an interrupt to stop the process.
                api::send_and_return(&GdbCmd::Interrupt.generate())
                    .unwrap()
                    .to(Target::Session(sid))
                    .await
                    .unwrap();

                let signal_cmd =
                    GdbCmd::ConsoleExec(format!("signal {}", cmd.args.trim())).generate();
                if let Ok(sender) = api::send_and_forget(&signal_cmd) {
                    match sender.to(cmd.target) {
                        Ok(_) => {
                            debug!("-send-signal command sent: {}", signal_cmd);
                        }
                        Err(e) => {
                            error!(
                                "Failed to send -send-signal command. raw: {}, err: {}",
                                signal_cmd, e
                            );
                        }
                    }
                } else {
                    error!("Failed to send -send-signal command. raw: {}", signal_cmd);
                }
            }
            _ => {
                error!("-send-signal command should specify a session");
            }
        }
    }
}

pub struct ListSignalsHandler;

#[async_trait]
impl Handler for ListSignalsHandler {
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if !cmd.args.trim().is_empty() {
            error!("-list-signals command needs no argument. raw: {}", cmd.args);
            return;
        }

        match cmd.target {
            Target::Session(_) => {
                // TODO: use signal mgr to cache results?
                // so that we can directly return if it is cached already.
                let list_signal_cmd = format!(
                    "{}-list-signals",
                    cmd.external_token.map(|token| token.to_string()).unwrap_or("".to_string())
                );
                if let Ok(sender) = api::send(&list_signal_cmd) {
                    match sender.to::<false>(cmd.target) {
                        Ok(_) => {
                            debug!("-list-signals command sent: {}", list_signal_cmd);
                        }
                        Err(e) => {
                            error!(
                                "Failed to send -list-signals command. raw: {}, err: {}",
                                list_signal_cmd, e
                            );
                        }
                    }
                } else {
                    error!(
                        "Failed to send -list-signals command. raw: {}",
                        list_signal_cmd
                    );
                }
            }
            _ => {
                error!("-list-signals command should specify a session");
            }
        }
    }
}
