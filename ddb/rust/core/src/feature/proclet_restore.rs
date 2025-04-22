use anyhow::{bail, Context, Result};
use tracing::debug;

use crate::{
    cmd_flow::{
        get_router,
        input::{Command, ParsedInputCmd},
        NullFormatter,
    }, common::{config::Framework, Config}, get_dbg_mgr, state::get_proclet_mgr
};

#[derive(Debug)]
struct ProcletHeapInfo {
    start_addr: u64,
    data_len: u64,
    data: String,
    full_heap_size: u64,
}

/// This is used during the distributed backtrace.
/// The goal here is to temporarily restore the proclet to original location.
/// We should keep states regarding where is a session proclet is restored so that we can properly clean up later.
pub struct ProcletRestorationMgr {}

impl ProcletRestorationMgr {
    pub fn new() -> Self {
        Self {}
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

    async fn query_proclet_location(&self, proclet_id: &String) -> Result<u32> {
        let proclet_id = proclet_id.parse::<u64>().unwrap();
        let resp = get_dbg_mgr()
            .query_proclet(proclet_id)
            .await
            .with_context(|| format!("Failed to query proclet {} from ProcletMgr", proclet_id))?;
        Ok(resp.caladan_ip)
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
        let caladan_ip = self.query_proclet_location(&proclet_id).await?;
        let owner_sid = get_proclet_mgr()
            .get_sid(caladan_ip)
            .ok_or(anyhow::anyhow!(
                "Fail to find the owner session for proclet {}. caladan_ip: {}",
                proclet_id,
                caladan_ip
            ))?;
        let heap_info = self.get_proclet_heap(owner_sid, &proclet_id).await?;
        self.restore_proclet_heap(sid, &heap_info).await?;

        Ok(())
    }

    pub async fn handle_proclet_restoration(&self, sid: u64, proclet_id: &String) -> Result<()> {
        // let g_cfg = Config::global();
        // match g_cfg.framework {
        //     Framework::Quicksand | Framework::Nu => {}
        //     _ => { return Ok(()); }
        // }
        // if !g_cfg.conf.support_migration {
        //     return Ok(());
        // }

        if proclet_id.is_empty() || proclet_id == "0" {
            bail!("Invalid proclet id: {}", proclet_id);
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

        let is_local = self.check_proclet_local(sid, &proclet_id).await?;

        if !is_local {
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

        Ok(())
    }

    fn gen_cmd(cmd: &str) -> Command<NullFormatter> {
        TryInto::<ParsedInputCmd>::try_into(cmd)
            .unwrap()
            .to_command(NullFormatter)
            .1
    }
}
