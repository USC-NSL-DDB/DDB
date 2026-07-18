use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use dashmap::DashMap;
use flume;
use gdbmi::{
    parser::{Message, Response},
    raw::Dict,
    Token,
};
use serde::Serialize;
use tokio::sync::oneshot;
use tracing::{debug, error, trace, warn};

use crate::{
    dbg_parser::gdb_parser::{GdbParser, MIFormatter},
    get_dbg_mgr,
    state::{get_bkpt_mgr, ThreadStatus, STATES},
};

use super::{
    emit, emit_static, DynFormatter, GenericStopAsyncRecordFormatter, RunningAsyncRecordFormatter,
    StopAsyncRecordFormatter, ThreadCreatedNotifFormatter, ThreadExitedNotifFormatter,
    ThreadGroupNotifFormatter,
};

/// Determines the destination for command output
///
/// # Variants
/// - `STDOUT`: Output routed to standard output (default CLI behavior)
/// - `RETURN`: Output collected and returned via oneshot channel (for API calls awaiting results)
/// - `DISCARD`: Output discarded (used for internal commands)
#[derive(Debug)]
pub enum OutputSource {
    STDOUT,
    RETURN(oneshot::Sender<FinishedCmd>),
    DISCARD,
}

/// Parsed response from a single GDB session
///
/// Contains the session ID, response message, and optional payload data.
/// This is the basic unit of response tracking before aggregation.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedSessionResponse {
    sid: u64,
    message: String,
    payload: Option<Dict>,
}

impl ParsedSessionResponse {
    fn new(sid: u64, message: String, payload: Option<Dict>) -> Self {
        Self {
            sid,
            message,
            payload,
        }
    }

    pub fn get_sid(&self) -> u64 {
        self.sid
    }

    pub fn get_message(&self) -> &String {
        &self.message
    }

    pub fn get_payload(&self) -> Option<&Dict> {
        self.payload.as_ref()
    }

    pub fn get_payload_mut(&mut self) -> Option<&mut Dict> {
        self.payload.as_mut()
    }

    pub fn to_finished_cmd(self, external_token: Option<u64>, sid: u64) -> FinishedCmd {
        FinishedCmd::new(external_token, sid, vec![self])
    }
}

/// Response wrapper containing session ID and raw response bytes
#[derive(Clone)]
pub struct SessionResponse {
    sid: u64,
    response: Bytes,
}

impl SessionResponse {
    pub fn new(sid: u64, response: Bytes) -> Self {
        Self { sid, response }
    }
}

impl Debug for SessionResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionResponse")
            .field("sid", &self.sid)
            // .field("token", &self.token)
            .field("response", &std::str::from_utf8(&self.response))
            .finish()
    }
}

/// Aggregated command completion containing all responses from target sessions
///
/// # Stability Contract
/// - Public accessor methods (`get_*`) are stable and will not change
/// - Internal representation may change but is hidden behind accessors
/// - Used as return value for `Router::send_to_ret` operations
#[derive(Debug, Serialize, Clone)]
pub struct FinishedCmd {
    external_token: Option<u64>,
    sid: u64,
    responses: Vec<ParsedSessionResponse>,
}

impl FinishedCmd {
    pub fn new(
        external_token: Option<u64>,
        sid: u64,
        responses: Vec<ParsedSessionResponse>,
    ) -> Self {
        Self {
            external_token,
            sid,
            responses,
        }
    }
    pub fn set_external_token(&mut self, external_token: u64) {
        self.external_token = Some(external_token);
    }
    pub fn get_external_token(&self) -> Option<u64> {
        self.external_token
    }

    pub fn get_sid(&self) -> u64 {
        self.sid
    }

    pub fn get_responses(&self) -> &Vec<ParsedSessionResponse> {
        &self.responses
    }

    pub fn get_responses_mut(&mut self) -> &mut Vec<ParsedSessionResponse> {
        self.responses.as_mut()
    }
}

pub struct OutgoingCmd {
    // The Difference between `id` and `ext_id`:
    // For example, when user inputs `123-thread-info`, we will first take out the user-input token (`123`)
    // and then generate a new token (`456`, for example) and prepend to the gdb command.
    // Therefore, we effectively send `456-thread-info` to all sessions. We use this new token to track
    // the response from gdb and then send it back to the user with the original token.
    // Here, `123` is the ext_id and `456` is the id.
    id: u64,             // essentially the token we inserted in gdb command
    ext_id: Option<u64>, // the token users use in their input gdb commands

    // The command is not needed here, but it can be helpful for debugging
    // e.g. print out all inflight commands (with their raw command).
    cmd: String,

    target_num_resp: u32,
    received_num_resp: u32,
    responses: Vec<ParsedSessionResponse>,
    out_src: OutputSource,
    formatter: Box<dyn DynFormatter>,
    created_at: Instant,
}

// This is a copy of OutgoingCmd without the out_src and formatter field.
// This is used for API server and for debugging purpose
pub struct OutgoingCmdCpy {
    pub id: u64,
    pub ext_id: Option<u64>,
    pub cmd: String,
    pub target_num_resp: u32,
    pub received_num_resp: u32,
    pub responses: Vec<ParsedSessionResponse>,
}

impl Into<OutgoingCmdCpy> for OutgoingCmd {
    fn into(self) -> OutgoingCmdCpy {
        OutgoingCmdCpy {
            id: self.id,
            ext_id: self.ext_id,
            cmd: self.cmd,
            target_num_resp: self.target_num_resp,
            received_num_resp: self.received_num_resp,
            responses: self.responses,
        }
    }
}

impl Into<OutgoingCmdCpy> for &OutgoingCmd {
    fn into(self) -> OutgoingCmdCpy {
        OutgoingCmdCpy {
            id: self.id,
            ext_id: self.ext_id,
            cmd: self.cmd.clone(),
            target_num_resp: self.target_num_resp,
            received_num_resp: self.received_num_resp,
            responses: self.responses.clone(),
        }
    }
}

impl Debug for OutgoingCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutgoingCmd")
            .field("id", &self.id)
            .field("ext_id", &self.ext_id)
            .field("target_num_resp", &self.target_num_resp)
            .field("received_num_resp", &self.received_num_resp)
            .field("responses", &self.responses)
            .field("out_src", &self.out_src)
            .field("formatter", &self.formatter_type())
            .finish()
    }
}

impl OutgoingCmd {
    pub fn new<F: DynFormatter + 'static>(
        id: u64,
        ext_id: Option<u64>,
        cmd: String,
        target_num_resp: u32,
        out_src: OutputSource,
        formatter: F,
    ) -> Self {
        Self {
            id,
            ext_id,
            cmd,
            target_num_resp,
            received_num_resp: 0,
            responses: Vec::with_capacity(target_num_resp as usize),
            out_src,
            formatter: Box::new(formatter),
            created_at: Instant::now(),
        }
    }

    fn recv_response(&mut self, response: &ParsedSessionResponse) -> bool {
        self.received_num_resp += 1;
        self.responses.push(response.clone());
        self.is_ready()
    }

    fn is_ready(&self) -> bool {
        self.received_num_resp == self.target_num_resp
    }

    fn formatter_type(&self) -> String {
        let type_name = std::any::type_name_of_val(&*self.formatter);
        type_name.to_string()
    }

    fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= COMMAND_TIMEOUT
    }
}

pub struct Tracker {
    inflight_cmds: DashMap<u64, OutgoingCmd>, // id (internal token) -> OutgoingCmd
    worker_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    senders: DashMap<u64, flume::Sender<SessionResponse>>,
}

const RESPONSE_QUEUE_CAPACITY: usize = 1024;
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

impl Tracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inflight_cmds: DashMap::new(),
            // response_rx: rx,
            worker_handles: Mutex::new(Vec::new()),
            senders: DashMap::new(),
        })
    }

    pub fn start(self: Arc<Self>, num_workers: usize) {
        let mut handles = self.worker_handles.lock().unwrap();

        for idx in 0..num_workers {
            // A full response queue propagates pressure to the session reader,
            // keeping a stalled parser from growing memory indefinitely.
            let (tx, rx) = flume::bounded::<SessionResponse>(RESPONSE_QUEUE_CAPACITY);

            let tracker = Arc::clone(&self);
            let handle = tokio::spawn(async move {
                tracker.worker(rx).await;
            });
            handles.push(handle);
            self.senders.insert(idx as u64, tx);
        }

        let tracker = Arc::clone(&self);
        handles.push(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(COMMAND_SWEEP_INTERVAL);
            loop {
                ticker.tick().await;
                tracker.expire_stale_commands();
            }
        }));
    }

    #[inline]
    pub fn register_output_tx(&self, id: u64) -> flume::Sender<SessionResponse> {
        let num_tx = self.senders.len();
        let idx = id % num_tx as u64;
        self.senders
            .get(&idx)
            .expect("valid sender")
            .value()
            .clone()
    }

    #[inline]
    pub fn add_cmd(&self, cmd: OutgoingCmd) {
        if cmd.target_num_resp == 0 {
            // edge case: send to no one, so drop it immediately
            return;
        }
        self.inflight_cmds.insert(cmd.id, cmd);
    }

    pub fn cancel_cmd(&self, token: u64) {
        self.inflight_cmds.remove(&token);
    }

    fn expire_stale_commands(&self) {
        let expired = self
            .inflight_cmds
            .iter()
            .filter(|entry| entry.value().is_expired())
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        for token in expired {
            if self.inflight_cmds.remove(&token).is_some() {
                warn!(
                    "Expired command token {} after {:?}",
                    token, COMMAND_TIMEOUT
                );
            }
        }
    }

    pub fn stop(&self) {
        let handles = self.worker_handles.lock().unwrap();
        for handle in handles.iter() {
            handle.abort();
        }
    }

    // get a copy of the inflight commands
    // this is a costy operation.
    pub fn get_inflight_cmds_copy(&self) -> Vec<OutgoingCmdCpy> {
        self.inflight_cmds
            .iter()
            .map(|entry| entry.value().into())
            .collect()
    }

    #[inline]
    fn handle_finished_cmd(cmd: OutgoingCmd) {
        let finished_cmd = FinishedCmd::new(cmd.ext_id, cmd.responses[0].sid, cmd.responses);
        match cmd.out_src {
            OutputSource::RETURN(tx) => match tx.send(finished_cmd) {
                Ok(_) => {}
                Err(e) => {
                    error!(
                        "Failed to send the finished command back to the caller: {:?}",
                        e
                    );
                }
            },
            OutputSource::DISCARD => {}
            OutputSource::STDOUT => {
                emit(finished_cmd, cmd.formatter);
            }
        }
    }

    #[inline]
    async fn process_parsed_response(&self, msg: Message, sid: u64) {
        match msg {
            Message::Response(resp) => {
                trace!("handle response: {:?}", resp);
                match resp {
                    Response::Notify {
                        token,
                        message,
                        payload,
                    } => {
                        self.handle_notify(token, message, payload, sid).await;
                    }
                    Response::Result {
                        token,
                        message,
                        payload,
                    } => {
                        self.handle_result(token, message, payload, sid);
                    }
                }
            }
            Message::General(msg) => {
                trace!("General message: {:?}", msg);
            }
        }
    }

    async fn worker(&self, receiver: flume::Receiver<SessionResponse>) {
        loop {
            match receiver.recv_async().await {
                Ok(resp) => {
                    let outputs = std::str::from_utf8(&resp.response)
                        .map(|resp_str| GdbParser::parse_multiple(resp_str));

                    match outputs {
                        Ok(outputs) => {
                            for output in outputs {
                                self.process_parsed_response(output, resp.sid).await;
                            }
                        }
                        Err(e) => {
                            error!("Response is not a valid utf8 string: {:?}", e);
                        }
                    }
                }
                Err(_) => {
                    // Handle shutdown case if needed
                    break;
                }
            }
        }
    }

    async fn handle_notify(&self, token: Option<Token>, message: String, payload: Dict, sid: u64) {
        let token = token.map(|t| t.0 as u64);
        match message.as_str() {
            "breakpoint-modified" => {
                // no-op for now
                // TODO: update bkpt hit count here.
            }
            "breakpoint-deleted" => {
                // seems like this path will only be trigged when a temporary breakpoint is deleted.
                // manually and explicitly removing a breakpoint will not trigger this notification.
                let local_bkpt_id = payload
                    .get("id")
                    .and_then(|id_val| Some(id_val.expect_string_repr::<u64>().unwrap()));
                if let Some(local_bkpt_id) = local_bkpt_id {
                    get_bkpt_mgr().delete_local_bkpt(sid, local_bkpt_id).await;
                } else {
                    warn!(
                        "breakpoint-deleted notification missing id field: {:?}",
                        payload
                    );
                }
            }
            "thread-created" => {
                let tgid = payload["group-id"].expect_string_ref().unwrap();
                let tid = payload["id"].expect_string_repr::<u64>().unwrap();
                let (gtid, gtgid) = STATES.create_thread(sid, tid, &tgid).await;
                let service_meta = STATES.session_service_meta(sid).await;
                debug!("service_meta: {:?}", service_meta);

                let resp = ParsedSessionResponse::new(sid, message, Some(payload));

                emit_static(
                    resp.to_finished_cmd(token, sid),
                    ThreadCreatedNotifFormatter::new(gtid, gtgid, sid, service_meta),
                );
            }
            "thread-exited" => {
                let tid = payload["id"].expect_string_repr::<u64>().unwrap();
                let tgid = payload["group-id"].expect_string_ref().unwrap();

                let gtid = STATES.remove_thread(sid, tid).expect(
                    format!(
                        "Thread exit failed. Thread not found. sid: {}, tid: {}",
                        sid, tid
                    )
                    .as_str(),
                );
                let gtgid = STATES.global_thread_group_id(sid, tgid).expect(
                    format!("Thread group not found. sid: {}, tgid: {}", sid, tgid).as_str(),
                );
                let resp = ParsedSessionResponse::new(sid, message, Some(payload));

                emit_static(
                    resp.to_finished_cmd(token, sid),
                    ThreadExitedNotifFormatter::new(gtid, gtgid, sid),
                );
            }
            "running" => {
                let tid = payload["thread-id"].expect_string_ref().unwrap();
                if tid == "all" {
                    STATES
                        .update_all_thread_status(sid, ThreadStatus::RUNNING)
                        .await;
                    let pending = ParsedSessionResponse::new(sid, message, Some(payload))
                        .to_finished_cmd(token, sid);
                    emit_static(pending, RunningAsyncRecordFormatter::new(true));
                } else {
                    let tid = tid.parse::<u64>().unwrap();
                    STATES
                        .update_thread_status(sid, tid, ThreadStatus::RUNNING)
                        .await;
                    let pending = ParsedSessionResponse::new(sid, message, Some(payload))
                        .to_finished_cmd(token, sid);
                    emit_static(pending, RunningAsyncRecordFormatter::new(false));
                }
            }
            "stopped" => {
                let payload = payload.clone();
                if let Some(reason) = payload.get("reason") {
                    if let Some(reason_str) = reason.expect_string_ref().ok() {
                        // clean up the session via DbgMgr. As per the current design,
                        // it will
                        // 1. remove from router.
                        // 2. shutdown the connection.
                        // 3. remove from all related states.
                        if reason_str.contains("exit") {
                            get_dbg_mgr().remove_session(sid).await;
                            return;
                        }
                    }
                    if let Some(reason_list) = reason.expect_list_ref().ok() {
                        // This can happen when gdb is configured to
                        // use `pass` and `nostop` for some signals
                        // e.g., `handle SIGINT pass nostop`
                        // In this case, the reason will be a list of reasons.
                        // e.g., List([String("signal-received"), String("exited-signalled")])
                        for r in reason_list {
                            if let Some(r_str) = r.expect_string_ref().ok() {
                                if r_str.contains("exit") {
                                    get_dbg_mgr().remove_session(sid).await;
                                    return;
                                }
                            }
                        }
                    }
                }

                if let Some(tid) = payload.get("thread-id") {
                    let tid = tid.expect_string_ref().unwrap();
                    if tid == "all" {
                        STATES
                            .update_all_thread_status(sid, ThreadStatus::STOPPED)
                            .await;
                    } else {
                        let tid = tid.parse::<u64>().unwrap();
                        STATES
                            .update_thread_status(sid, tid, ThreadStatus::STOPPED)
                            .await;

                        // Here, we assume it runs in all-stop mode.
                        // Therefore, when a thread hits a breakpoint,
                        // all threads stops and the currently stopped thread
                        // as the current selected thread automatically.
                        if payload
                            .get("reason")
                            .and_then(|r| r.expect_string_ref().ok())
                            .map_or("none", |s| s)
                            == "breakpoint-hit"
                        {
                            STATES.select_local_thread(sid, tid).await;
                        }
                    }

                    if let Some(stopped_threads) = payload.get("stopped-threads") {
                        if stopped_threads.expect_string_ref().unwrap() == "all" {
                            STATES
                                .update_all_thread_status(sid, ThreadStatus::STOPPED)
                                .await;
                        } else {
                            // Handle non-stop mode where threads may stop at different times
                            for tid in stopped_threads.expect_list_ref().unwrap() {
                                let tid = tid.expect_string_repr::<u64>().unwrap();
                                STATES
                                    .update_thread_status(sid, tid, ThreadStatus::STOPPED)
                                    .await;
                            }
                        }
                        let resp = ParsedSessionResponse::new(sid, message, Some(payload));
                        emit_static(resp.to_finished_cmd(token, sid), StopAsyncRecordFormatter);
                    } else {
                        warn!(
                            "Stopped message does not contain stopped-threads field: {:?}",
                            payload
                        );
                    }
                } else {
                    let resp = ParsedSessionResponse::new(sid, message, Some(payload));
                    emit_static(
                        resp.to_finished_cmd(token, sid),
                        GenericStopAsyncRecordFormatter,
                    );
                }
            }
            "thread-group-added" => {
                let tgid = payload["id"].expect_string_ref().unwrap();
                let gtgid = STATES.add_thread_group(sid, &tgid).await;
                let resp = ParsedSessionResponse::new(sid, message, Some(payload));
                emit_static(
                    resp.to_finished_cmd(token, sid),
                    ThreadGroupNotifFormatter::new(gtgid),
                );
            }
            "thread-group-removed" => {
                let tgid = payload["id"].expect_string_ref().unwrap();
                let gtgid = STATES.remove_thread_group(sid, &tgid).await.expect(
                    format!("Thread group not found. sid: {}, tgid: {}", sid, tgid).as_str(),
                );
                let resp = ParsedSessionResponse::new(sid, message, Some(payload));
                emit_static(
                    resp.to_finished_cmd(token, sid),
                    ThreadGroupNotifFormatter::new(gtgid),
                );
            }
            "thread-group-started" => {
                let tgid = payload["id"].expect_string_ref().unwrap();
                let pid = payload["pid"].expect_string_repr::<u64>().unwrap();
                let gtgid = STATES.start_thread_group(sid, &tgid, pid).await.unwrap();
                let resp = ParsedSessionResponse::new(sid, message, Some(payload));
                emit_static(
                    resp.to_finished_cmd(token, sid),
                    ThreadGroupNotifFormatter::new(gtgid),
                );
            }
            "thread-group-exited" => {
                let tgid = payload["id"].expect_string_ref().unwrap();
                let gtgid = STATES.exit_thread_group(sid, &tgid).await.unwrap();
                let resp = ParsedSessionResponse::new(sid, message, Some(payload));
                emit_static(
                    resp.to_finished_cmd(token, sid),
                    ThreadGroupNotifFormatter::new(gtgid),
                );
            }
            _ => {
                trace!("Unhandled notify message: {:?}", message);
            }
        }
    }

    fn handle_result(
        &self,
        token: Option<Token>,
        message: String,
        payload: Option<Dict>,
        sid: u64,
    ) {
        match token {
            None => {
                let result = MIFormatter::format("^", &message, payload.as_ref(), None);
                trace!("result with no token: {:?}", result);
            }
            Some(token) => {
                let resp = ParsedSessionResponse::new(sid, message.clone(), payload.clone());
                let token = token.0 as u64;
                if let Some(mut cmd) = self.inflight_cmds.get_mut(&token) {
                    // TODO: check if all response is really needed?
                    // Maybe we can just pass in CoW Bytes for the performance consideration
                    if cmd.recv_response(&resp) {
                        // To avoid UB (or deadlock potentially?), drop the mutable reference before removing the entry
                        drop(cmd);
                        if let Some(removed_cmd) = self.inflight_cmds.remove(&token) {
                            trace!("Command {:?} is ready", removed_cmd.1);
                            Self::handle_finished_cmd(removed_cmd.1);
                        }
                    }
                }
            }
        }
    }
}
