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
        api::{self, CommandExecutor},
        decoder::{OperationStatus, ProcletHeap, ProcletLocality},
        session_runtime::SessionLease,
        transaction::SessionTransaction,
    },
    get_dbg_mgr,
    state::ProcletMgr,
};

type ProcletId = u64;

#[derive(Debug, Clone)]
struct ProcletLoc {
    sid: u64,
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
    proclets: Arc<ProcletMgr>,
    executor: CommandExecutor,
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
    pub fn new(proclets: Arc<ProcletMgr>, executor: CommandExecutor) -> Self {
        Self {
            proclets,
            executor,
            proclet_is_local_cache: DashMap::new(),
            proclet_loc_cache: DashMap::new(),
            proclet_restored_heap_meta: DashMap::new(),
        }
    }

    fn transaction_lease(
        transaction: Option<&SessionTransaction>,
        sid: u64,
    ) -> Result<Option<&SessionLease>> {
        match transaction {
            Some(transaction) => transaction
                .lease_for(sid)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("transaction does not reserve session {}", sid)),
            None => Ok(None),
        }
    }

    pub async fn related_session(&self, target_sid: u64, proclet_id: &str) -> Result<Option<u64>> {
        let owner = self.query_proclet_location(proclet_id).await?.sid;
        Ok((owner != target_sid).then_some(owner))
    }

    async fn check_proclet_local(
        &self,
        sid: u64,
        proclet_id: &str,
        transaction: Option<&SessionTransaction>,
    ) -> Result<bool> {
        let resp = self
            .executor
            .execute_plan_with_optional_lease(
                api::command(&format!("-check-proclet {}", proclet_id))?
                    .target(api::Target::Session(sid))
                    .protocol_complete(),
                Self::transaction_lease(transaction, sid)?,
            )
            .await
            .with_context(|| format!("Failed to send -check-proclet command to session {}", sid))?;
        Ok(ProcletLocality::decode(&resp)?.is_local)
    }

    async fn query_proclet_location(&self, proclet_id: &str) -> Result<ProcletLoc> {
        let proclet_id = proclet_id
            .parse::<u64>()
            .with_context(|| format!("Invalid proclet id: {}", proclet_id))?;
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
        let owner_sid =
            self.proclets
                .session_id_for_caladan_ip(caladan_ip)
                .ok_or(anyhow::anyhow!(
                    "Fail to find the owner session for proclet {}. caladan_ip: {}",
                    proclet_id,
                    caladan_ip
                ))?;
        let proc_loc = ProcletLoc { sid: owner_sid };
        *loc_guard = Some(proc_loc.clone());
        Ok(proc_loc)
    }

    async fn get_proclet_heap(
        &self,
        target_sid: u64,
        proclet_id: &str,
        transaction: Option<&SessionTransaction>,
    ) -> Result<ProcletHeapInfo> {
        let resp = self
            .executor
            .execute_plan_with_optional_lease(
                api::command(&format!("-get-proclet-heap {}", proclet_id))?
                    .target(api::Target::Session(target_sid))
                    .protocol_complete(),
                Self::transaction_lease(transaction, target_sid)?,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to send -get-proclet-heap command to session {}",
                    target_sid
                )
            })?;
        let heap = ProcletHeap::decode(&resp)?.validate()?;
        Ok(ProcletHeapInfo {
            start_addr: heap.start_addr,
            data_len: heap.data_len,
            data: heap.data,
            full_heap_size: heap.full_heap_size,
            proclet_id: proclet_id.to_string(),
        })
    }

    async fn restore_proclet_heap(
        &self,
        sid: u64,
        heap_info: &ProcletHeapInfo,
        transaction: Option<&SessionTransaction>,
    ) -> Result<()> {
        let resp = self
            .executor
            .execute_plan_with_optional_lease(
                api::command(&format!(
                    "-restore-proclet-heap {} {} {}",
                    heap_info.start_addr, heap_info.data_len, heap_info.data
                ))?
                .target(api::Target::Session(sid)),
                Self::transaction_lease(transaction, sid)?,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to send -get-proclet-heap command to session {}",
                    sid
                )
            })?;
        OperationStatus::decode(&resp)?.require_success("restore-proclet-heap")?;
        Ok(())
    }

    async fn get_and_restore_proclet_heap(
        &self,
        sid: u64,
        proclet_id: &str,
        transaction: Option<&SessionTransaction>,
    ) -> Result<()> {
        let proclet_loc = self.query_proclet_location(proclet_id).await?;
        let heap_info = self
            .get_proclet_heap(proclet_loc.sid, proclet_id, transaction)
            .await?;
        self.restore_proclet_heap(sid, &heap_info, transaction)
            .await?;
        let mut per_session_set = self.proclet_restored_heap_meta.entry(sid).or_default();
        per_session_set.insert(heap_info.into());
        Ok(())
    }

    pub async fn handle_proclet_restoration(
        &self,
        sid: u64,
        proclet_id: &str,
        transaction: Option<&SessionTransaction>,
    ) -> Result<()> {
        if proclet_id.is_empty() || proclet_id == "0" {
            bail!("Invalid proclet id: {}", proclet_id);
        }

        // if not local, we hold the mutex and restore the proclet heap.
        // here, intentionally hold the mutex so that the repeated request can be throttled automatically.
        // the major goal is to avoid excessive network calls to the proclet manager.
        let proclet_id_u64 = proclet_id
            .parse::<u64>()
            .with_context(|| format!("Invalid proclet id: {}", proclet_id))?;
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

        if !self
            .check_proclet_local(sid, proclet_id, transaction)
            .await?
        {
            debug!(
                "Proclet {} is not local on session {}. Restoring heap...",
                proclet_id, sid
            );
            self.get_and_restore_proclet_heap(sid, proclet_id, transaction)
                .await?;
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

        let futs = cloned_heap_meta
            .iter()
            .map(|(sid, heap_set)| async move {
                self.cleanup_heap_for(*sid, heap_set).await;
            })
            .collect::<Vec<_>>();

        futures::future::join_all(futs).await;
    }

    async fn _cleanup_heap_for(&self, sid: u64, h: &ProcletHeapMeta) -> Result<()> {
        let resp = self
            .executor
            .execute_plan(
                api::command(&format!(
                    "-clean-proclet-heap {} {}",
                    h.proclet_id, h.full_heap_size
                ))?
                .target(api::Target::Session(sid)),
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to send -cleanup-proclet-heap command to session {}",
                    sid
                )
            })?;
        OperationStatus::decode(&resp)?.require_success("clean-proclet-heap")?;
        Ok(())
    }

    async fn cleanup_heap_for(&self, sid: u64, heap_set: &HashSet<ProcletHeapMeta>) {
        for h in heap_set {
            match self._cleanup_heap_for(sid, h).await {
                Ok(_) => {
                    trace!(
                        "Proclet heap {} cleaned up on session {}",
                        h.proclet_id,
                        sid
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
}
