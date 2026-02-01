use std::{collections::HashSet, sync::Arc};

use anyhow::{bail, Result};
use dashmap::DashMap;
use serde::Deserialize;
use tracing::{debug, error, info, warn};

use super::{
    get_cmd_tracker,
    input::{Command, ParsedInputCmd},
    DynFormatter, FinishedCmd, OutputSource, PlainFormatter, Tracker,
};
use crate::{
    cmd_flow::NullFormatter, dbg_ctrl::InputSender, get_dbg_mgr, state::{
        GroupId, LocalThreadId, STATES, get_bkpt_mgr, get_group_mgr, get_proclet_mgr, get_source_mgr
    }
};

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

pub struct Router {
    sessions: DashMap<u64, InputSender>,
    tracker: Arc<Tracker>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            tracker: get_cmd_tracker(),
        }
    }

    pub fn add_session(&self, sid: u64, session_input_tx: InputSender) {
        self.sessions.insert(sid, session_input_tx);
    }

    pub fn remove_session(&self, sid: u64) {
        self.sessions.remove(&sid);
    }
}

impl Router {
    fn write_to(&self, sid: u64, cmd_str: String) {
        debug!("Router writing to session: {}, command: {}", sid, cmd_str);
        if let Some(session) = self.sessions.get(&sid) {
            let s = session.clone();
            drop(session);
            tokio::spawn(async move {
                match s.send_async(cmd_str.into()).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Router failed to send command to session: {:?}", e);
                    }
                }
            });
        } else {
            warn!("Router attempted to write to non-existent session: {}", sid);
        }
    }

    fn write_to_all(&self, cmd_str: String) {
        debug!("Router writing to all sessions. command {}", cmd_str);
        for session in self.sessions.iter() {
            let s = session.clone();
            let cmd = cmd_str.clone();
            drop(session);

            tokio::spawn(async move {
                match s.send_async(cmd.into()).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Router failed to send command to session: {:?}", e);
                    }
                }
            });
        }
    }

    pub fn send_to<F: DynFormatter + Clone>(&self, target: Target, cmd: Command<F>) {
        match target {
            Target::Session(sid) => self.send_to_session::<F, false>(sid, cmd),
            Target::Thread(gtid) => self.send_to_thread::<F, false>(gtid, cmd),
            Target::CurrThread => self.send_to_current_thread::<F, false>(cmd),
            Target::CurrSession => self.send_to_current_session::<F, false>(cmd),
            Target::Broadcast => self.broadcast::<F, false>(cmd),
            Target::First => self.send_to_first::<F, false>(cmd),
            Target::SessionSet(sids) => self.send_to_session_set::<F, false>(&sids, cmd),
            Target::Group(gid) => self.send_to_group::<F, false>(gid, cmd),
            Target::Multiple(targets) => {
                self.send_to_multiple::<F, false>(targets, cmd);
            }
        }
    }

    pub async fn send_to_ret<F: DynFormatter + Clone>(
        &self,
        target: Target,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        match target {
            Target::Session(sid) => self.send_to_session_ret(sid, cmd).await,
            Target::Thread(gtid) => self.send_to_thread_ret(gtid, cmd).await,
            Target::CurrThread => self.send_to_current_thread_ret(cmd).await,
            Target::CurrSession => self.send_to_current_session_ret(cmd).await,
            Target::Broadcast => self.broadcast_ret(cmd).await,
            Target::First => self.send_to_first_ret(cmd).await,
            Target::SessionSet(sids) => self.send_to_session_set_ret(&sids, cmd).await,
            Target::Group(gid) => self.send_to_group_ret(gid, cmd).await,
            Target::Multiple(targets) => self.send_to_multiple_ret(targets, cmd).await,
        }
    }

    pub fn send_to_and_forget(&self, target: Target, cmd: Command<NullFormatter>) {
        type F = NullFormatter;
        match target {
            Target::Session(sid) => self.send_to_session::<F, true>(sid, cmd),
            Target::Thread(gtid) => self.send_to_thread::<F, true>(gtid, cmd),
            Target::CurrThread => self.send_to_current_thread::<F, true>(cmd),
            Target::CurrSession => self.send_to_current_session::<F, true>(cmd),
            Target::Broadcast => self.broadcast::<F, true>(cmd),
            Target::First => self.send_to_first::<F, true>(cmd),
            Target::SessionSet(sids) => self.send_to_session_set::<F, true>(&sids, cmd),
            Target::Group(gid) => self.send_to_group::<F, true>(gid, cmd),
            Target::Multiple(targets) => {
                self.send_to_multiple::<F, true>(targets, cmd);
            }
        }
    }
    
    pub fn send_to_multiple<F: DynFormatter + Clone, const FORGET: bool>(&self, targets: Vec<Target>, cmd: Command<F>)
    {
        for target in targets {
            if FORGET {
                self.send_to_and_forget(target, cmd.clone().into_null_formatter_cmd());
            } else {
                self.send_to(target, cmd.clone());
            }
        }
    }

    pub async fn send_to_multiple_ret<F: DynFormatter + Clone>(&self, targets: Vec<Target>, cmd: Command<F>) -> Result<FinishedCmd> {
        let futures: Vec<_> = targets.into_iter().map(|target| {
            let cmd = cmd.clone();
            async move {
                self.send_to_ret(target, cmd).await
            }
        }).collect();
        let results = futures::future::join_all(futures).await.into_iter().collect::<Result<Vec<_>>>()?;
        if results.is_empty() {
            bail!("No results from send_to_multiple_ret");
        }
        if results.len() == 1 {
            return Ok(results.into_iter().next().unwrap());
        }
        // Combine all FinishedCmd into one
        let mut head_cmd = results[0].clone();
        let combined_resps = head_cmd.get_responses_mut();
        for resp in results.into_iter().skip(1) {
            combined_resps.extend(resp.get_responses().clone());
        }
        Ok(head_cmd)
    }

    pub fn send_to_group<F: DynFormatter + Clone, const FORGET: bool>(&self, gid: GroupId, cmd: Command<F>) {
        if let Some(grp) = get_group_mgr().get_grp_by_id(gid) {
            self.send_to_session_set::<F, FORGET>(grp.get_sids(), cmd);
        } else {
            warn!("Group (id: {}) doesn't exist", gid);
        }
    }

    pub async fn send_to_group_ret<F: DynFormatter + Clone>(
        &self,
        gid: GroupId,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        if let Some(grp) = get_group_mgr().get_grp_by_id(gid) {
            self.send_to_session_set_ret(grp.get_sids(), cmd).await
        } else {
            bail!("Group (id: {}) doesn't exist", gid);
        }
    }

    pub fn send_to_session_set<F: DynFormatter + Clone, const FORGET: bool>(&self, sids: &HashSet<u64>, cmd: Command<F>) {
        // perform some sanity checks to remove all non-existent sessions
        let sids = sids
            .iter()
            .filter(|sid| self.sessions.contains_key(sid))
            .collect::<Vec<_>>();
        
        let out_src = if FORGET {
            OutputSource::DISCARD
        } else {
            OutputSource::STDOUT
        };

        let (out_meta, cmd) = cmd.prepare_to_send(sids.len() as u32, out_src);
        if !FORGET {
            self.tracker.add_cmd(out_meta);
        }

        for sid in sids {
            self.write_to(*sid, cmd.clone());
        }
    }

    pub async fn send_to_session_set_ret<F: DynFormatter + Clone>(
        &self,
        sids: &HashSet<u64>,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        // perform some sanity checks to remove all non-existent sessions
        let sids = sids
            .iter()
            .filter(|sid| self.sessions.contains_key(sid))
            .collect::<Vec<_>>();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let out_src = OutputSource::RETURN(tx);
        let (out_meta, cmd) = cmd.prepare_to_send(sids.len() as u32, out_src);
        self.tracker.add_cmd(out_meta);

        for sid in sids {
            self.write_to(*sid, cmd.clone());
        }
        Ok(rx.await?)
    }

    pub fn send_to_session<F: DynFormatter + Clone, const FORGET: bool>(&self, sid: u64, cmd: Command<F>) {
        STATES.set_curr_session(sid);
        let out_src = if FORGET {
            OutputSource::DISCARD
        } else {
            OutputSource::STDOUT
        };
        let (out_meta, cmd) = cmd.prepare_to_send(1, out_src);
        if !FORGET {
            self.tracker.add_cmd(out_meta);
        }
        self.write_to(sid, cmd);
    }

    pub async fn send_to_session_ret<F: DynFormatter + Clone>(
        &self,
        sid: u64,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        self.sessions.get(&sid).ok_or_else(|| anyhow::anyhow!("Session {} does not exist", sid))?;
        STATES.set_curr_session(sid);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let out_src = OutputSource::RETURN(tx);
        let (out_meta, cmd) = cmd.prepare_to_send(1, out_src);
        self.tracker.add_cmd(out_meta);
        self.write_to(sid, cmd);
        Ok(rx.await?)
    }

    pub fn send_to_thread<F: DynFormatter + Clone, const FORGET: bool>(&self, gtid: u64, cmd: Command<F>) {
        if let Some(LocalThreadId(sid, tid)) = STATES.get_ltid_by_gtid(gtid) {
            STATES.set_curr_session(sid);
            let out_src = if FORGET {
                OutputSource::DISCARD
            } else {
                OutputSource::STDOUT
            };
            let (out_meta, cmd) = cmd.prepare_to_send(1, out_src);
            if !FORGET {
                self.tracker.add_cmd(out_meta);
            }
            self.write_to(sid, format!("-thread-select {}\n{}", tid, cmd));
        } 
    }

    pub async fn send_to_thread_ret<F: DynFormatter + Clone>(
        &self,
        gtid: u64,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        if let Some(LocalThreadId(sid, tid)) = STATES.get_ltid_by_gtid(gtid) {
            STATES.set_curr_session(sid);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let out_src = OutputSource::RETURN(tx);
            let (out_meta, cmd) = cmd.prepare_to_send(1, out_src);
            self.tracker.add_cmd(out_meta);
            self.write_to(sid, format!("-thread-select {}\n{}", tid, cmd));

            Ok(rx.await?)
        } else {
            bail!("Thread (gtid: {}) is not in a session group", gtid);
        }
    }

    pub fn send_to_current_thread<F: DynFormatter + Clone, const FORGET: bool>(&self, cmd: Command<F>) {
        if let Some(gtid) = STATES.get_curr_gtid() {
            self.send_to_thread::<F, FORGET>(gtid, cmd);
        } else {
            println!("use -thread-select #gtid to select the thread first.");
        }
    }

    pub async fn send_to_current_thread_ret<F: DynFormatter + Clone>(
        &self,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        if let Some(gtid) = STATES.get_curr_gtid() {
            self.send_to_thread_ret(gtid, cmd).await
        } else {
            bail!("use -thread-select #gtid to select the thread first.");
        }
    }

    pub fn send_to_current_session<F: DynFormatter + Clone, const FORGET: bool>(&self, cmd: Command<F>) {
        if let Some(sid) = STATES.get_curr_session() {
            self.send_to_session::<F, FORGET>(sid, cmd);
        }
    }

    pub async fn send_to_current_session_ret<F: DynFormatter + Clone>(
        &self,
        cmd: Command<F>,
    ) -> Result<FinishedCmd> {
        if let Some(sid) = STATES.get_curr_session() {
            return self.send_to_session_ret(sid, cmd).await;
        }
        bail!("No current session selected.");
    }

    pub fn broadcast<F: DynFormatter + Clone, const FORGET: bool>(&self, cmd: Command<F>) {
        let num_sessions = self.sessions.len() as u32;
        let out_src = if FORGET {
            OutputSource::DISCARD
        } else {
            OutputSource::STDOUT
        };
        let (out_meta, cmd) = cmd.prepare_to_send(num_sessions, out_src);
        if !FORGET {
            self.tracker.add_cmd(out_meta);
        }
        self.write_to_all(cmd);
    }

    pub async fn broadcast_ret<F: DynFormatter + Clone>(&self, cmd: Command<F>) -> Result<FinishedCmd> {
        let num_sessions = self.sessions.len() as u32;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let out_src = OutputSource::RETURN(tx);
        let (out_meta, cmd) = cmd.prepare_to_send(num_sessions, out_src);
        self.tracker.add_cmd(out_meta);
        self.write_to_all(cmd);
        Ok(rx.await?)
    }

    pub fn send_to_first<F: DynFormatter + Clone, const FORGET: bool>(&self, cmd: Command<F>) {
        if let Some(s) = self.sessions.iter().next() {
            let sid = s.key().clone();
            drop(s);
            self.send_to_session::<F, FORGET>(sid, cmd);
        } else {
            error!("No session available.");
        }
    }

    pub async fn send_to_first_ret<F: DynFormatter + Clone>(&self, cmd: Command<F>) -> Result<FinishedCmd> {
        if let Some(s) = self.sessions.iter().next() {
            let sid = s.key().clone();
            drop(s);
            self.send_to_session_ret(sid, cmd).await
        } else {
            bail!("No session available.");
        }
    }

    pub fn handle_internal_cmd(&self, cmd: &str) {
        if cmd == "p-session-meta" {
            info!("p-session-meta: {:?}", STATES.get_all_sessions())
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
                let (_, cmd) = parsed.to_command(PlainFormatter);
                self.send_to_session::<PlainFormatter, false>(sid, cmd);
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
