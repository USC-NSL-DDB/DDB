use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use gdbmi::raw::{Dict, Value};
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

use crate::{
    cmd_flow::{emit_error, transaction},
    common::Config,
    dbg_parser::gdb_parser::{bkpt_deleted_payload, MIFormatter},
    debugger::get_debugger_backend,
    feature::get_proclet_restore_mgr,
    notification::{get_notif_mgr, BreakpointChangeEvent, Notification, NotificationPayload},
    state::{
        get_bkpt_mgr, get_group_mgr, get_state_mgr, BkptLoc, BreakpointStateChange, GroupSubBkpt,
        LocalThreadId, SessionRef, SessionSubBkpt, SubBkptType, ThreadContext, ThreadStatus,
        STATES,
    },
};

use super::{
    api, framework_adapter::FrameworkCommandAdapter, input::ParsedInputCmd, output, router::Target,
    DebuggerDataErr, FinishedCmd, NullFormatter, PlainFormatter, ProcessReadableFormatter,
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
/// Handlers use `crate::cmd_flow::api::parsed` to construct a semantic request,
/// then choose `execute`, `emit`, or `submit` explicitly.
#[async_trait]
pub trait Handler: Send + Sync + std::fmt::Debug {
    async fn process_cmd(&self, cmd: ParsedInputCmd);
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let _ = api::parsed(cmd).unwrap().emit(PlainFormatter).await;
    }
}

#[derive(Debug)]
pub struct BreakInsertHandler;
impl BreakInsertHandler {
    pub fn new() -> Self {
        BreakInsertHandler
    }
}

impl BreakInsertHandler {
    fn parse_breakpoint_location(args: &str) -> Result<BkptLoc> {
        let location = args
            .trim()
            .rsplit_once(char::is_whitespace)
            .map(|(_, tail)| tail)
            .unwrap_or(args)
            .trim_matches(['"', '\'']);
        let (src, line) = location.rsplit_once(':').ok_or_else(|| {
            anyhow!(
                "Unsupported breakpoint location '{}'. Expected <file>:<line>.",
                location
            )
        })?;
        if src.is_empty() {
            bail!("Breakpoint source path cannot be empty");
        }
        let line = line
            .parse::<u64>()
            .map_err(|_| anyhow!("Invalid breakpoint line '{}'", line))?;
        Ok(BkptLoc::new(src, line))
    }

    async fn insert_bkpts_for_group(major_bkpt_id: u64, cmd: &str, gid: u64) -> Result<()> {
        let mut grp_bkpt = GroupSubBkpt::new(gid);

        // Check if group exists and has active sessions
        let grp = match get_group_mgr().group_by_id(gid) {
            Some(g) => g,
            None => {
                warn!("Group {} does not exist", gid);
                return Err(anyhow!("Group {} does not exist", gid));
            }
        };
        let sids = grp.session_ids();

        // Only send breakpoint command if group has active sessions
        // If empty, the breakpoint will be applied later when sessions join via sync_bkpts_state()
        if !sids.is_empty() {
            let ret = api::command(cmd)
                .unwrap()
                .target(Target::Group(gid))
                .execute()
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
                let _times = bkpt_info
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
        get_bkpt_mgr().add_sub_breakpoint(major_bkpt_id, subbkpt);
        Ok(())
    }

    async fn insert_bkpts_for_session(major_bkpt_id: u64, cmd: &str, sid: u64) -> Result<()> {
        let ret = api::command(cmd)
            .unwrap()
            .target(Target::Session(sid))
            .execute()
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
        let _times = bkpt_info
            .get("times")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        // TODO: work out how to store `times` information.
        let subbkpt = SubBkptType::Session(SessionSubBkpt::new(local_bkpt_id, sid));
        get_bkpt_mgr().add_sub_breakpoint(major_bkpt_id, subbkpt);
        Ok(())
    }
}

#[async_trait]
impl Handler for BreakInsertHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
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
        let bkpt_loc = match Self::parse_breakpoint_location(args) {
            Ok(location) => location,
            Err(error) => {
                emit_error(&error.to_string(), cmd.external_token);
                return;
            }
        };
        let bkpt_id = get_bkpt_mgr().add_breakpoint(bkpt_loc);

        match cmd.target {
            Target::Session(sid) => {
                if let Err(e) = Self::insert_bkpts_for_session(bkpt_id, &full_cmd, sid).await {
                    get_bkpt_mgr().remove_breakpoint(bkpt_id);
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
                    get_bkpt_mgr().remove_breakpoint(bkpt_id);
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
                            .group_by_id(*gid)
                            .map(|grp| grp.session_ids().clone()),
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
                get_bkpt_mgr().remove_breakpoint(bkpt_id);
                warn!(
                    "Unsupported target for BreakInsertHandler: {:?}",
                    cmd.target
                );
                return;
            }
        }

        if get_bkpt_mgr().breakpoint_is_empty(bkpt_id) == Some(true) {
            get_bkpt_mgr().remove_breakpoint(bkpt_id);
            emit_error(
                "Failed to insert breakpoint into any target.",
                cmd.external_token,
            );
            return;
        }

        match get_bkpt_mgr().breakpoint(bkpt_id) {
            Some(bkpt) => {
                let out = MIFormatter::format(
                    "^",
                    "done",
                    Some(&bkpt.clone().into()),
                    cmd.external_token,
                );
                println!("{}", out);
                debug!("output: {}", out);

                get_notif_mgr()
                    .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                        BreakpointChangeEvent::Added((&bkpt).into()),
                    )))
                    .await;
            }
            None => {
                warn!("Failed to find inserted breakpoint with id {}", bkpt_id);
            }
        }
    }
}

#[derive(Debug)]
pub struct BreakDeleteHandler;
impl BreakDeleteHandler {
    pub fn new() -> Self {
        BreakDeleteHandler
    }
}

impl BreakDeleteHandler {
    async fn delete_local_bkpt(sid: u64, local_bkpt_id: u64) -> Result<BreakpointStateChange> {
        let ret = api::command(&format!("-break-delete {}", local_bkpt_id))
            .unwrap()
            .target(Target::Session(sid))
            .execute()
            .await?;
        let response = ret.get_responses().first().unwrap();
        if response.get_message() == "done" {
            Ok(get_bkpt_mgr().record_local_bkpt_deletion(sid, local_bkpt_id))
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

    fn merge_state_change(
        current: BreakpointStateChange,
        next: BreakpointStateChange,
    ) -> BreakpointStateChange {
        match (current, next) {
            (BreakpointStateChange::Removed(bkpt_id), _)
            | (_, BreakpointStateChange::Removed(bkpt_id)) => {
                BreakpointStateChange::Removed(bkpt_id)
            }
            (BreakpointStateChange::TargetChanged(bkpt_id), _)
            | (_, BreakpointStateChange::TargetChanged(bkpt_id)) => {
                BreakpointStateChange::TargetChanged(bkpt_id)
            }
            _ => BreakpointStateChange::None,
        }
    }

    fn finalize_explicit_subbkpt_delete(
        bkpt_id: u64,
        subbkpt_id: u64,
    ) -> Result<BreakpointStateChange> {
        get_bkpt_mgr().remove_sub_breakpoint(bkpt_id, subbkpt_id);
        match get_bkpt_mgr().breakpoint_is_empty(bkpt_id) {
            Some(true) => {
                get_bkpt_mgr().remove_breakpoint(bkpt_id);
                Ok(BreakpointStateChange::Removed(bkpt_id))
            }
            Some(false) => Ok(BreakpointStateChange::TargetChanged(bkpt_id)),
            None => Ok(BreakpointStateChange::None),
        }
    }

    async fn delete_sub_breakpoint(bkpt_id: u64, subbkpt_id: u64) -> Result<BreakpointStateChange> {
        if let Some(subbkpt) = get_bkpt_mgr().sub_breakpoint(bkpt_id, subbkpt_id) {
            match subbkpt.kind() {
                SubBkptType::Session(sess_subbkpt) => {
                    let sid = sess_subbkpt.target_session();
                    let local_bkpt_id = sess_subbkpt.local_id();
                    let ret = Self::delete_local_bkpt(sid, local_bkpt_id).await;
                    match ret {
                        Ok(change) => return Ok(change),
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
                    let local_ids = group_subbkpt.local_ids();
                    if local_ids.is_empty() {
                        return Self::finalize_explicit_subbkpt_delete(bkpt_id, subbkpt_id);
                    }

                    let mut change = BreakpointStateChange::None;
                    for (sid, local_bkpt_id) in local_ids {
                        let ret = Self::delete_local_bkpt(sid, local_bkpt_id).await;
                        match ret {
                            Ok(local_change) => {
                                change = Self::merge_state_change(change, local_change);
                            }
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
                        return Ok(change);
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
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let args = cmd.args.trim();
        if args.is_empty() {
            warn!("No breakpoint id provided for deletion.");
            return;
        }

        let bkpt_id: u64;
        if let Some((bkpt_id_str, subbkpt_id_str)) = args.split_once(char::is_whitespace) {
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
            match Self::delete_sub_breakpoint(bkpt_id, subbkpt_id).await {
                Ok(BreakpointStateChange::TargetChanged(bkpt_id)) => {
                    if let Some(bkpt) = get_bkpt_mgr().breakpoint(bkpt_id) {
                        let out = MIFormatter::format("^", "done", None, cmd.external_token);
                        println!("{}", out);
                        debug!("output: {}", out);

                        let out = MIFormatter::format(
                            "=",
                            "breakpoint-modified",
                            Some(&bkpt.clone().into()),
                            None,
                        );
                        println!("{}", out);
                        debug!("output: {}", out);

                        get_notif_mgr()
                            .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                                BreakpointChangeEvent::Updated((&bkpt).into()),
                            )))
                            .await;
                    }
                }
                Ok(BreakpointStateChange::Removed(bkpt_id)) => {
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

                    get_notif_mgr()
                        .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                            BreakpointChangeEvent::Removed(bkpt_id),
                        )))
                        .await;
                }
                Ok(BreakpointStateChange::None) => {}
                Err(e) => {
                    warn!(
                        "Failed to delete sub-breakpoint {} of breakpoint {}: {:?}",
                        subbkpt_id, bkpt_id, e
                    );
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
            for (sid, local_bkpt_id) in get_bkpt_mgr().local_breakpoint_ids(bkpt_id) {
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

            get_bkpt_mgr().remove_breakpoint(bkpt_id);
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

            get_notif_mgr()
                .broadcast(Notification::new(NotificationPayload::BreakpointChanged(
                    BreakpointChangeEvent::Removed(bkpt_id),
                )))
                .await;
        }
    }
}

#[derive(Debug)]
pub struct ThreadInfoHandler;

impl ThreadInfoHandler {
    pub fn new() -> Self {
        ThreadInfoHandler
    }
}

#[async_trait]
impl Handler for ThreadInfoHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
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
                let _ = api::command(&thrd_info_cmd)
                    .unwrap()
                    .target(Target::Thread(tid))
                    .emit(ThreadInfoFormatter)
                    .await;
            }
            _ => {
                let _ = api::parsed(cmd).unwrap().emit(ThreadInfoFormatter).await;
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
    async fn continue_session(cont_cmd: ParsedInputCmd, session: SessionRef) -> Result<()> {
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
                return Ok(());
            };
            let restore = api::command(&format!(
                "-switch-context-custom {}",
                Self::prepare_ctx_switch_args(&ctx)
            ))
            .unwrap()
            .target(Target::Thread(ctx.tid))
            .execute_exclusive(tx.lease())
            .await?;
            let responses = restore.get_responses();
            let restored = responses.len() == 1
                && responses[0].get_payload().unwrap()["message"]
                    .expect_string_ref()
                    .unwrap()
                    == "success";

            session
                .write_with(|meta| meta.set_in_custom_context(!restored))
                .await;

            if !restored {
                bail!("Failed to restore context for session {}", sid);
            }
        }

        let _ = api::parsed(cont_cmd)
            .unwrap()
            .target(Target::Session(sid))
            .emit_exclusive(tx.lease(), PlainFormatter)
            .await;
        session
            .write_with(|meta| meta.update_all_status(ThreadStatus::RUNNING))
            .await;
        Ok(())
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if Config::global().conf.support_migration {
            // reset all proclet cache and clean up restored proclet heap.
            get_proclet_restore_mgr().reset().await;
        }

        let tasks: Vec<JoinHandle<Result<()>>> = match &cmd.target {
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

        for result in futures::future::join_all(tasks).await {
            match result {
                Err(e) => error!("Failed to continue: {:?}", e),
                Ok(Err(e)) => error!("Failed to continue: {:?}", e),
                Ok(Ok(())) => {}
            }
        }
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let cmd = cmd.with_prefix("-exec-interrupt-if-running");
        match cmd.target {
            Target::Session(sid) => {
                let ss = STATES.session(sid);
                if ss.is_some() {
                    // Note: send interrupt to running process. Ignore thread granularity.
                    // skips checking if the thread is running or not.
                    let _ = api::parsed(cmd).unwrap().emit(PlainFormatter).await;
                }
            }
            _ => {
                // broadcast to all sessions
                let _ = api::parsed(cmd)
                    .unwrap()
                    .target(Target::Broadcast)
                    .emit(PlainFormatter)
                    .await;
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        // FIXME: a naive implementation here, just select the first session
        // This command is need for CLI (to list out sources), but probably not for GUI?
        STATES.select_session(1);
        let _ = api::parsed(cmd)
            .unwrap()
            .target(Target::CurrSession)
            .emit(PlainFormatter)
            .await;
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let parts = cmd.args.split_whitespace().collect::<Vec<_>>();
        if !parts.is_empty() {
            let gtid = parts.last().unwrap().parse::<u64>().unwrap();
            let (sid, tid) = STATES.local_thread_id(gtid).unwrap().into();
            let target = Target::Session(sid);
            let _ = api::command(&format!("-thread-select {}", tid))
                .unwrap()
                .target(target)
                .emit(PlainFormatter)
                .await;
        } else {
            let _ = api::parsed(cmd).unwrap().emit(PlainFormatter).await;
        }
    }
}

#[derive(Debug)]
pub struct ListGroupsHandler;

impl ListGroupsHandler {
    pub fn new() -> Self {
        ListGroupsHandler
    }
}

#[async_trait]
impl Handler for ListGroupsHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        let _ = api::parsed(cmd)
            .unwrap()
            .target(Target::Broadcast)
            .emit(ProcessReadableFormatter)
            .await;
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
            output::emit_static(out_result, PlainFormatter);
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if let Target::Thread(_) = &cmd.target {
            let _ = api::parsed(cmd).unwrap().emit(NullFormatter).await;
        } else {
            error!("exec-next command should specify a thread id by --thread <gtid>");
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if let Target::Thread(_) = &cmd.target {
            let _ = api::parsed(cmd).unwrap().emit(NullFormatter).await;
        } else {
            error!("exec-finish command should specify a thread id by --thread <gtid>");
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
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if let Target::Thread(_) = &cmd.target {
            let _ = api::parsed(cmd).unwrap().emit(NullFormatter).await;
        } else {
            error!("exec-step command should specify a thread id by --thread <gtid>");
        }
    }
}

#[derive(Debug)]
pub struct ExecJumpHandler;

#[async_trait]
impl Handler for ExecJumpHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        // Note: `exec-jump` should only be used when session is specified at the moment.
        // otherwise it will be ambiguous which process to jump to.
        // let (target, cmd) = cmd.to_command(PlainFormatter);
        match cmd.target {
            Target::Session(_) => {
                let _ = api::parsed(cmd).unwrap().emit(PlainFormatter).await;
            }
            _ => {
                error!("exec-jump command should specify a session");
            }
        }
    }
}

#[derive(Debug)]
pub struct SendSignalHandler;

#[async_trait]
impl Handler for SendSignalHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
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
                let backend = get_debugger_backend();
                api::command(&backend.interrupt_command())
                    .unwrap()
                    .target(Target::Session(sid))
                    .execute()
                    .await
                    .unwrap();

                let signal_cmd =
                    backend.console_exec_command(&format!("signal {}", cmd.args.trim()));
                match api::command(&signal_cmd) {
                    Ok(request) => match request.target(Target::Session(sid)).submit().await {
                        Ok(()) => debug!("-send-signal command sent: {}", signal_cmd),
                        Err(error) => error!(
                            "Failed to send -send-signal command. raw: {}, err: {}",
                            signal_cmd, error
                        ),
                    },
                    Err(error) => error!(
                        "Failed to parse -send-signal command. raw: {}, err: {}",
                        signal_cmd, error
                    ),
                }
            }
            _ => {
                error!("-send-signal command should specify a session");
            }
        }
    }
}

#[derive(Debug)]
pub struct ListSignalsHandler;

#[async_trait]
impl Handler for ListSignalsHandler {
    #[cfg_attr(feature = "profile", tracing::instrument(skip(self)))]
    async fn process_cmd(&self, cmd: ParsedInputCmd) {
        if !cmd.args.trim().is_empty() {
            error!("-list-signals command needs no argument. raw: {}", cmd.args);
            return;
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
                match api::command(&list_signal_cmd) {
                    Ok(request) => match request.target(Target::Session(sid)).emit_plain().await {
                        Ok(()) => debug!("-list-signals command sent: {}", list_signal_cmd),
                        Err(error) => error!(
                            "Failed to send -list-signals command. raw: {}, err: {}",
                            list_signal_cmd, error
                        ),
                    },
                    Err(error) => error!(
                        "Failed to parse -list-signals command. raw: {}, err: {}",
                        list_signal_cmd, error
                    ),
                }
            }
            _ => {
                error!("-list-signals command should specify a session");
            }
        }
    }
}
