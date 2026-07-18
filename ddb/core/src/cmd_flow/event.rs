use gdbmi::{raw::Dict, Token};
use tracing::{debug, trace, warn};

use crate::{
    get_dbg_mgr,
    state::{get_bkpt_mgr, ThreadStatus, STATES},
};

use super::{
    emit_static, GenericStopAsyncRecordFormatter, ParsedSessionResponse,
    RunningAsyncRecordFormatter, StopAsyncRecordFormatter, ThreadCreatedNotifFormatter,
    ThreadExitedNotifFormatter, ThreadGroupNotifFormatter,
};

pub(crate) async fn project_event(token: Option<Token>, message: String, payload: Dict, sid: u64) {
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
                .map(|id_val| id_val.expect_string_repr::<u64>().unwrap());
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
            let (gtid, gtgid) = STATES.create_thread(sid, tid, tgid).await;
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

            let gtid = STATES.remove_thread(sid, tid).unwrap_or_else(|| {
                panic!(
                    "Thread exit failed. Thread not found. sid: {}, tid: {}",
                    sid, tid
                )
            });
            let gtgid = STATES
                .global_thread_group_id(sid, tgid)
                .unwrap_or_else(|| panic!("Thread group not found. sid: {}, tgid: {}", sid, tgid));
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
                if let Ok(reason_str) = reason.expect_string_ref() {
                    // clean up the session via DbgMgr. As per the current design,
                    // it will
                    // 1. remove from router.
                    // 2. shutdown the connection.
                    // 3. remove from all related states.
                    if reason_str.contains("exit") {
                        tokio::spawn(async move {
                            get_dbg_mgr().remove_session(sid).await;
                        });
                        return;
                    }
                }
                if let Ok(reason_list) = reason.expect_list_ref() {
                    // This can happen when gdb is configured to
                    // use `pass` and `nostop` for some signals
                    // e.g., `handle SIGINT pass nostop`
                    // In this case, the reason will be a list of reasons.
                    // e.g., List([String("signal-received"), String("exited-signalled")])
                    for r in reason_list {
                        if let Ok(r_str) = r.expect_string_ref() {
                            if r_str.contains("exit") {
                                tokio::spawn(async move {
                                    get_dbg_mgr().remove_session(sid).await;
                                });
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
            let gtgid = STATES.add_thread_group(sid, tgid).await;
            let resp = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                resp.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        "thread-group-removed" => {
            let tgid = payload["id"].expect_string_ref().unwrap();
            let gtgid = STATES
                .remove_thread_group(sid, tgid)
                .await
                .unwrap_or_else(|| panic!("Thread group not found. sid: {}, tgid: {}", sid, tgid));
            let resp = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                resp.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        "thread-group-started" => {
            let tgid = payload["id"].expect_string_ref().unwrap();
            let pid = payload["pid"].expect_string_repr::<u64>().unwrap();
            let gtgid = STATES.start_thread_group(sid, tgid, pid).await.unwrap();
            let resp = ParsedSessionResponse::new(sid, message, Some(payload));
            emit_static(
                resp.to_finished_cmd(token, sid),
                ThreadGroupNotifFormatter::new(gtgid),
            );
        }
        "thread-group-exited" => {
            let tgid = payload["id"].expect_string_ref().unwrap();
            let gtgid = STATES.exit_thread_group(sid, tgid).await.unwrap();
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
