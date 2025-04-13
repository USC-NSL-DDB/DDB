use std::path::Path;

use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use tracing::debug;

use super::{DbgMode, DbgSessionConfig};
use crate::cmd_flow::{get_router, SessionResponse};
use crate::dbg_cmd::{GdbCmd, GdbOption};
use crate::dbg_ctrl::{InputSender, OutputReceiver};
use crate::state::{get_group_mgr, STATES};
use crate::{cmd_flow, common};
use crate::{
    common::default_vals::{DEFAULT_GDB_EXT_DIR, DEFAULT_GDB_EXT_NAME},
    dbg_cmd::DbgCmdListBuilder,
    session::DbgStartMode,
};

// Prefer static dispatch over dynamic dispatch here
// considering we don't have too much flexibility in
// the controller implementation at the moment.
//
// If we need to support more controller types in the future,
// we can consider using dynamic dispatch.
#[derive(Debug)]
pub struct DbgSession {
    pub sid: u64,
    pub config: DbgSessionConfig,
    pub poll_handle: Option<tokio::task::JoinHandle<()>>,

    // for sending commands to the target process
    pub input_tx: Option<InputSender>,

    // for receiving output from the target process
    pub output_rx: Option<OutputReceiver>,
}

// pub struct DbgSessionRef {
//     pub tx: tokio::sync::mpsc::Sender<()>,
//     pub sid: u64,
// }

impl DbgSession {
    pub fn new(config: DbgSessionConfig) -> Self {
        use crate::common::counter::next_session_id;
        // Safety: ssh_cred is guaranteed to be Some
        // let ssh_cred = config.ssh_cred.clone().unwrap();
        // used to pass output from SSH connections back to the session.

        let sid = next_session_id();
        // let ctrl=config.gdb_controller.as_dyn();
        DbgSession {
            sid,
            config,
            poll_handle: None,
            input_tx: None,
            output_rx: None,
        }
    }

    #[allow(unused)]
    pub fn get_input_sender(&self) -> Option<InputSender> {
        self.input_tx.clone()
    }

    pub async fn start(&mut self) -> Result<InputSender> {
        // Note: need to register the session before starting it.
        // so that it has session meta entry to update if the start
        // process needs to update the session state.
        STATES
            .register_session(
                self.sid,
                self.config
                    .tag
                    .clone()
                    .unwrap_or(format!("session-{}", self.sid))
                    .as_str(),
                self.config.service_meta.clone(),
            )
            .await;

        let sender = match &self.config.mode {
            DbgMode::LOCAL(_) => self.local_start().await?,
            DbgMode::REMOTE(DbgStartMode::ATTACH(_)) => self.remote_attach().await?,
            DbgMode::REMOTE(DbgStartMode::BINARY(_)) => self.remote_start().await?,
        };

        // this procedure seems to be quite slow, so we can do it in the background.
        // taking this out of the critical path may have other implications...
        // TODO: ... need to think about this more.
        #[cfg(not(feature = "lazy_source_map"))]
        tokio::spawn(async move {
            // try to resolve the source files
            // this should be done after updating the router,
            // as it will try to use router to send to a specific session.
            match get_source_mgr().resolve_src_for(new_sid).await {
                Ok(_) => {
                    debug!("Source files resolved successfully.");
                }
                Err(e) => {
                    debug!("Failed to resolve source files: {:?}", e);
                }
            }
        });

        // update the group manager
        if let Some(meta) = &self.config.service_meta {
            get_group_mgr().add_session(meta.hash.clone(), meta.alias.clone(), self.sid);
        }
        self.sync_state().await?;
        // update router with input sender
        get_router().add_session(self.sid, sender.clone());
        let output_rx = self.output_rx.clone().unwrap();
        self.poll_handle = Some(tokio::spawn(Self::poll(self.sid, output_rx)));
        // Update session status
        STATES.update_session_status_on(self.sid).await;
        Ok(sender)
    }

    /// Sync the state of the session with the binary group if any.
    /// For example, existing breakpoints are automatically inserted.
    ///
    /// Note: When this function is called, the group manager should
    /// have the latest updates, a.k.a., the current session is added to
    /// the group.
    pub async fn sync_state(&self) -> Result<()> {
        if let Some(grp_id) = get_group_mgr().get_group_id_by_sid(self.sid) {
            // insert existing breakpoints
            if let Some(bkpts) = crate::state::get_bkpt_mgr().get(&grp_id) {
                debug!("Inserting existing breakpoints: {:?}", bkpts);
                let bkpts_cmd = bkpts
                    .iter()
                    .map(|bkpt| bkpt.get_cmd().trim())
                    .collect::<Vec<_>>();
                let bkpts_cmd = bkpts_cmd.join("\n");
                let bkpts_cmd = if !bkpts_cmd.ends_with('\n') {
                    format!("{}\n", bkpts_cmd)
                } else {
                    bkpts_cmd
                };
                debug!("breakpoints commands: {:?}", bkpts_cmd);
                self.write(bkpts_cmd)
                    .await
                    .context("failed to write commands when inserting bkpts.")?;
            }
        }
        Ok(())
    }

    pub async fn remote_attach(&mut self) -> Result<InputSender> {
        use crate::common::config::{Config, Framework};
        use crate::common::utils::gdb::gdb_start_cmd;

        let full_args = gdb_start_cmd(Config::global().conf.sudo);
        let ctrl = &mut self.config.gdb_controller;
        let ssh_io = ctrl.start(&full_args).await?;
        self.input_tx = Some(ssh_io.in_tx.clone());
        self.output_rx = Some(ssh_io.out_rx.clone());
        let mut bdr = DbgCmdListBuilder::<GdbCmd>::new();
        bdr.add(GdbCmd::SetOption(GdbOption::Logging(true)));
        bdr.add(GdbCmd::SetOption(GdbOption::MiAsync(true)));

        match Config::global().framework {
            Framework::GRPC | Framework::Nu => {
                let gdb_ext_path = Path::new(DEFAULT_GDB_EXT_DIR).join(DEFAULT_GDB_EXT_NAME);
                bdr.add(GdbCmd::ConsoleExec(format!(
                    r#"source {}"#,
                    gdb_ext_path.to_str().unwrap()
                )));
            }
            _ => {}
        }

        for cmd in &self.config.prerun_gdb_cmds {
            bdr.add(cmd);
        }

        match &self.config.mode {
            DbgMode::REMOTE(DbgStartMode::ATTACH(pid)) => {
                bdr.add(GdbCmd::TargetAttach(*pid));
            }
            _ => return Err(anyhow::anyhow!("Invalid mode for remote attach")),
        }

        if common::config::Config::global().service_discovery.is_some() {
            // Broker is enabled. We need to handle the SIG40 signal.
            bdr.add(GdbCmd::ConsoleExec("signal SIG40".to_string()));
        }

        for cmd in &self.config.postrun_gdb_cmds {
            bdr.add(cmd);
        }

        let all_cmds = bdr.build().join("");
        self.write(all_cmds).await?;
        Ok(ssh_io.in_tx.clone())
    }

    pub async fn remote_start(&mut self) -> Result<InputSender> {
        unimplemented!()
    }

    pub async fn local_start(&mut self) -> Result<InputSender> {
        unimplemented!()
    }

    pub async fn poll(sid: u64, output_rx: OutputReceiver) {
        let tx = cmd_flow::get_output_tx(sid);
        let mut buffer: BytesMut = BytesMut::new();
        loop {
            match output_rx.recv_async().await {
                Ok(data) => {
                    buffer.extend_from_slice(&data);

                    // Find the last occurrence of '\n'
                    if let Some(last_newline) = buffer.iter().rposition(|&b| b == b'\n') {
                        // Extract all bytes up to and including the last '\n'
                        let bytes_to_send = buffer.split_to(last_newline + 1);

                        tx.send_async(SessionResponse::new(sid, bytes_to_send.freeze()))
                            .await
                            .ok();
                    } else {
                        // No '\n' found, so we need to wait for more data
                        continue;
                    }
                }
                Err(e) => {
                    debug!("Failed to receive output: {}", e);
                    break;
                }
            }
        }
    }

    pub async fn cleanup(&mut self) -> Result<()> {
        debug!("Cleaning up session with config: {:?}", self.config);

        // Indicate that the session is closing. Used in API server.
        STATES.update_session_status_off(self.sid).await;

        // Update all relevant states
        // 1. remove from router
        // 2. remove from the group manager
        // 3. remove from state manager
        // 4. shutdown the connection
        get_router().remove_session(self.sid);
        get_group_mgr().remove_session(self.sid);
        STATES.remove_session(self.sid).await;
        let ctrl = &self.config.gdb_controller;
        if ctrl.is_open() {
            match common::config::Config::global().conf.on_exit {
                common::config::OnExit::DETACH => {
                    self.write("detach\n").await?;
                    debug!("Detaching from the target process");
                }
                common::config::OnExit::KILL => {
                    self.write("kill\n").await?;
                    debug!("Killing the target process");
                }
            }
        }
        self.write("exit\n").await?;

        // Workaround:
        // The SSH library doesn't flush all outgoing messages before disconnecting.
        // Therefore, we need to wait for a while before closing the connection.
        let mut retries = 0;
        while ctrl.is_open() && retries < 10 {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            retries += 1;
        }
        if ctrl.is_open() {
            debug!("Failed to close controller after 10 retries");
        }
        let ctrl = &mut self.config.gdb_controller;
        ctrl.close().await?;
        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
        }
        Ok(())
    }
}

impl DbgSession {
    // Note: keep this API private and available,
    // as it has a nice interface for writing commands.
    // It is intended to be used internally.
    // For external input, use `input_tx` directly.
    async fn write<U: Into<Bytes> + Send>(&self, cmd: U) -> Result<()> {
        self.input_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Input channel not set"))?
            .send_async(cmd.into())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send command: {}", e))
    }
}
