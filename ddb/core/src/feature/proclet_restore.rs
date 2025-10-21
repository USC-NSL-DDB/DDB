use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
// use papaya::HashMap;
use dashmap::DashMap;
use tracing::{debug, trace};

use crate::{
    cmd_flow::{
        get_router,
        input::{Command, ParsedInputCmd},
        NullFormatter,
    },
    get_dbg_mgr,
    state::get_proclet_mgr,
};

type ProcletId = u64;

#[derive(Debug, Clone)]
struct ProcletLoc {
    sid: u64,
    caladan_ip: u32,
}

#[derive(Debug, Hash, PartialEq, Eq)]
struct ProcletQueryTarget {
    sid: u64,
    proclet_id: ProcletId,
}

#[derive(Debug)]
struct ProcletHeapInfo {
    start_addr: u64,
    data_len: u64,
    data: String,
    full_heap_size: u64,
    proclet_id: String,
}

/// This is used to store the proclet heap information w/o the content.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
struct ProcletHeapMeta {
    start_addr: u64,
    data_len: u64,
    full_heap_size: u64,
    proclet_id: String,
}

impl From<ProcletHeapInfo> for ProcletHeapMeta {
    fn from(value: ProcletHeapInfo) -> Self {
        ProcletHeapMeta {
            start_addr: value.start_addr,
            data_len: value.data_len,
            full_heap_size: value.full_heap_size,
            proclet_id: value.proclet_id,
        }
    }
}

/// This is used during the distributed backtrace.
/// The goal here is to temporarily restore the proclet to original location.
/// We should keep states regarding where is a session proclet is restored so that we can properly clean up later.
pub struct ProcletRestorationMgr {
    // cache the result regarding whether the proclet is local to the session
    proclet_is_local_cache: DashMap<ProcletQueryTarget, Arc<tokio::sync::Mutex<bool>>>,
    // proclet_is_local_lock

    // cache the result regading the proclet location
    proclet_loc_cache: DashMap<ProcletId, Arc<tokio::sync::Mutex<Option<ProcletLoc>>>>,

    // if a heap is restored, we need to keep track of the metadata for future cleanup.
    // this is per session cache.
    proclet_restored_heap_meta: DashMap<u64, HashSet<ProcletHeapMeta>>,
}

impl ProcletRestorationMgr {
    pub fn new() -> Self {
        Self {
            proclet_is_local_cache: DashMap::new(),
            proclet_loc_cache: DashMap::new(),
            proclet_restored_heap_meta: DashMap::new(),
        }
    }

    async fn check_proclet_local(&self, sid: u64, proclet_id: &String) -> Result<bool> {
        let check_proclet_cmd = Self::gen_cmd(&format!("-check-proclet {}", proclet_id));

        let resp = get_router()
            .send_to_session_ret(sid, check_proclet_cmd)
            .await
            .with_context(|| format!("Failed to send -check-proclet command to session {}", sid))?;
        let payload = resp
            .get_responses()
            .get(0)
            .ok_or_else(|| anyhow::anyhow!("No response from check-proclet command. sid={}", sid))?
            .get_payload()
            .unwrap();
        let succcess = payload
            .get("success")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<bool>()
            .unwrap();

        let msg = payload
            .get("message")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .to_string();

        if !succcess {
            bail!(
                "Fail to check proclet ({}) on session {}. Err: {}",
                proclet_id,
                sid,
                msg
            );
        }

        let is_local = payload
            .get("is_local")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<bool>()
            .unwrap();

        // let status = payload
        //     .get("status")
        //     .unwrap()
        //     .expect_string_repr::<u32>()
        //     .unwrap();

        Ok(is_local)
    }

    async fn query_proclet_location(&self, proclet_id: &String) -> Result<ProcletLoc> {
        let proclet_id = proclet_id.parse::<u64>().unwrap();
        let loc_ref = self
            .proclet_loc_cache
            .entry(proclet_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
            .clone();
        let mut loc_guard = loc_ref.lock().await;
        if let Some(loc) = loc_guard.as_ref() {
            return Ok(loc.clone());
        }

        let resp = get_dbg_mgr()
            .query_proclet(proclet_id)
            .await
            .with_context(|| format!("Failed to query proclet {} from ProcletMgr", proclet_id))?;

        let caladan_ip = resp.caladan_ip;
        let owner_sid = get_proclet_mgr()
            .get_sid(caladan_ip)
            .ok_or(anyhow::anyhow!(
                "Fail to find the owner session for proclet {}. caladan_ip: {}",
                proclet_id,
                caladan_ip
            ))?;
        let proc_loc = ProcletLoc {
            sid: owner_sid,
            caladan_ip: resp.caladan_ip,
        };
        *loc_guard = Some(proc_loc.clone());
        Ok(proc_loc)
    }

    async fn get_proclet_heap(
        &self,
        target_sid: u64,
        proclet_id: &String,
    ) -> Result<ProcletHeapInfo> {
        let get_proclet_heap_cmd = Self::gen_cmd(&format!("-get-proclet-heap {}", proclet_id));
        let resp = get_router()
            .send_to_session_ret(target_sid, get_proclet_heap_cmd)
            .await
            .with_context(|| {
                format!(
                    "Failed to send -get-proclet-heap command to session {}",
                    target_sid
                )
            })?;
        let payload = resp
            .get_responses()
            .get(0)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No response from -get-proclet-heap command. sid={}",
                    target_sid
                )
            })?
            .get_payload()
            .unwrap();

        let success = payload
            .get("success")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<bool>()
            .unwrap();
        let msg = payload
            .get("message")
            .unwrap()
            .expect_string_ref()
            .unwrap_or_default()
            .to_string();

        if !success {
            bail!(
                "Fail to get proclet heap (proclet_id = {}) on session {}. Err: {}",
                proclet_id,
                target_sid,
                msg
            );
        }

        let start_addr = payload
            .get("start")
            .unwrap()
            .expect_string_repr::<u64>()
            .unwrap();

        let end_addr = payload
            .get("end")
            .unwrap()
            .expect_string_repr::<u64>()
            .unwrap();

        let data_len = payload
            .get("len")
            .unwrap()
            .expect_string_repr::<u64>()
            .unwrap();

        let full_heap_size = payload
            .get("full_heap_size")
            .unwrap()
            .expect_string_repr::<u64>()
            .unwrap();

        if start_addr == 0 || end_addr == 0 && data_len == 0 || full_heap_size == 0 {
            bail!(
                "Fail to get proclet heap (proclet_id = {}) on session {}. Err: {}",
                proclet_id,
                target_sid,
                msg
            );
        }

        let data = payload
            .get("heap_content")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .to_string();
        // .as_bytes();
        // let data = Bytes::copy_from_slice(data);

        Ok(ProcletHeapInfo {
            start_addr,
            data_len,
            data,
            full_heap_size,
            proclet_id: proclet_id.clone(),
        })
    }

    async fn restore_proclet_heap(&self, sid: u64, heap_info: &ProcletHeapInfo) -> Result<()> {
        let restore_proclet_heap_cmd = Self::gen_cmd(&format!(
            "-restore-proclet-heap {} {} {}",
            heap_info.start_addr, heap_info.data_len, heap_info.data
        ));
        let resp = get_router()
            .send_to_session_ret(sid, restore_proclet_heap_cmd)
            .await
            .with_context(|| {
                format!(
                    "Failed to send -get-proclet-heap command to session {}",
                    sid
                )
            })?;
        let payload = resp
            .get_responses()
            .get(0)
            .ok_or_else(|| {
                anyhow::anyhow!("No response from -get-proclet-heap command. sid={}", sid)
            })?
            .get_payload()
            .unwrap();

        let success = payload
            .get("success")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<bool>()
            .unwrap();
        let msg = payload
            .get("message")
            .unwrap()
            .expect_string_ref()
            .unwrap_or_default()
            .to_string();

        if !success {
            bail!(
                "Fail to restore proclet heap on session {}. Err: {}",
                sid,
                msg
            );
        }
        Ok(())
    }

    async fn get_and_restore_proclet_heap(&self, sid: u64, proclet_id: &String) -> Result<()> {
        let proclet_loc = self.query_proclet_location(&proclet_id).await?;
        let heap_info = self.get_proclet_heap(proclet_loc.sid, &proclet_id).await?;
        self.restore_proclet_heap(sid, &heap_info).await?;
        let mut per_session_set = self
            .proclet_restored_heap_meta
            .entry(sid)
            .or_insert_with(HashSet::new);
        per_session_set.insert(heap_info.into());
        Ok(())
    }

    pub async fn handle_proclet_restoration(&self, sid: u64, proclet_id: &String) -> Result<()> {
        if proclet_id.is_empty() || proclet_id == "0" {
            bail!("Invalid proclet id: {}", proclet_id);
        }

        // if not local, we hold the mutex and restore the proclet heap.
        // here, intentionally hold the mutex so that the repeated request can be throttled automatically.
        // the major goal is to avoid excessive network calls to the proclet manager.
        let proclet_id_u64 = proclet_id.parse::<u64>().unwrap();
        let is_local_ref = self
            .proclet_is_local_cache
            .entry(ProcletQueryTarget {
                sid,
                proclet_id: proclet_id_u64,
            })
            .or_insert(Arc::new(tokio::sync::Mutex::new(false)))
            .clone();
        let mut is_local_guard = is_local_ref.lock().await;
        if *is_local_guard {
            // is local, just return.
            return Ok(());
        }

        // TODO:
        // 1. check if the proclet is local on the parent session (get proclet_id)
        // 2. send `-check-proclet` to the parent session. input: proclet_id
        // 3. if not, need to restore the heap.
        //  a. query the proclet ctrl for the current location of the proclet. input: proclet_id
        //  b. read the caladan ip address from the proclet ctrl
        //  c. query the `ProcletMgr` to get the session id.
        //  d. send `-get-proclet-heap` to the session. input: proclet_id
        //  e. send `-restore-proclet-heap` to the current session. input: start_addr, data_len, data
        //  f. mark the heap is dirty and should clean it up upon continuing!!!!
        //

        if !self.check_proclet_local(sid, &proclet_id).await? {
            debug!(
                "Proclet {} is not local on session {}. Restoring heap...",
                proclet_id, sid
            );
            self.get_and_restore_proclet_heap(sid, &proclet_id).await?;
            debug!("Proclet {} heap restored on session {}", proclet_id, sid);
        } else {
            debug!(
                "Proclet {} is local on session {}. Skipping heap restoration.",
                proclet_id, sid
            );
        }

        // mark it as local now and drop the lock while returning.
        *is_local_guard = true;
        Ok(())
    }

    pub async fn reset(&self) {
        self.proclet_is_local_cache.clear();
        self.proclet_loc_cache.clear();
        self.cleanup_heap().await;
        self.proclet_restored_heap_meta.clear();
    }

    pub async fn cleanup_heap(&self) {
        // Clone the DashMap content into a regular HashMap to avoid holding DashMap references during async operations.
        let cloned_heap_meta: HashMap<u64, HashSet<ProcletHeapMeta>> = self
            .proclet_restored_heap_meta
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();
        
        let futs = cloned_heap_meta.iter().map(|(sid, heap_set)| async move {
            self.cleanup_heap_for(*sid, &heap_set).await;
        }).collect::<Vec<_>>();
        
        futures::future::join_all(futs).await;
    }

    async fn _cleanup_heap_for(&self, sid: u64, h: &ProcletHeapMeta) -> Result<()> {
        let clean_cmd = Self::gen_cmd(&format!(
            "-clean-proclet-heap {} {}",
            h.proclet_id, h.full_heap_size
        ));
        let resp = get_router()
            .send_to_session_ret(sid, clean_cmd)
            .await
            .with_context(|| {
                format!(
                    "Failed to send -cleanup-proclet-heap command to session {}",
                    sid
                )
            })?;
        let payload = resp
            .get_responses()
            .get(0)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No response from -cleanup-proclet-heap command. sid={}",
                    sid
                )
            })?
            .get_payload()
            .unwrap();
        let success = payload
            .get("success")
            .unwrap()
            .expect_string_ref()
            .unwrap()
            .parse::<bool>()
            .unwrap();

        let msg = payload
            .get("message")
            .unwrap()
            .expect_string_ref()
            .unwrap_or_default()
            .to_string();

        if !success {
            bail!(
                "Fail to cleanup proclet heap on session {}. Err: {}",
                sid,
                msg,
            );
        }
        Ok(())
    }

    async fn cleanup_heap_for(&self, sid: u64, heap_set: &HashSet<ProcletHeapMeta>) {
        for h in heap_set {
            match self._cleanup_heap_for(sid, h).await {
                Ok(_) => {
                    trace!(
                        "Proclet heap {} cleaned up on session {}",
                        h.proclet_id, sid
                    );
                }
                Err(e) => {
                    debug!(
                        "Failed to clean up proclet heap {} on session {}. Err: {}",
                        h.proclet_id, sid, e
                    );
                }
            }
        }
    }

    fn gen_cmd(cmd: &str) -> Command<NullFormatter> {
        TryInto::<ParsedInputCmd>::try_into(cmd)
            .unwrap()
            .to_command(NullFormatter)
            .1
    }
}
