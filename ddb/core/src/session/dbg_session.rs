use std::path::Path;

use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use tracing::debug;

use super::{DbgMode, DbgSessionConfig};
use crate::cmd_flow::{api, get_router, SessionResponse, Target};
use crate::common::Config;
use crate::common::default_vals::{DEFAULT_GDB_EXT_FRAME_FILTER_NAME, PROCLET_GDB_EXT_NAME};
use crate::dbg_cmd::{FrameFilterAddArgs, FrameFilterCmdArg, GdbCmd, GdbOption};
use crate::dbg_ctrl::{InputSender, OutputReceiver};
use crate::state::{STATES, get_bkpt_mgr, get_group_mgr};
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
            get_group_mgr().add_session(&meta.hash, meta.alias.clone(), self.sid);
        }
        // self.sync_state().await?;
        // update router with input sender
        get_router().add_session(self.sid, sender.clone());
        let output_rx = self.output_rx.clone().unwrap();
        self.poll_handle = Some(tokio::spawn(Self::poll(self.sid, output_rx)));
        // Update session status
        STATES.update_session_status_on(self.sid).await;
        Ok(sender)
    }

    pub async fn post_start(&self) -> Result<()> {
        // Sync state after starting the session.
        self.sync_bkpts_state().await?;
        
        // Instruct debuggee to continue if service discovery is enabled.
        let mut bdr = DbgCmdListBuilder::<GdbCmd>::new();
        if Config::global().service_discovery.is_some() {
            // Broker is enabled. We need to handle the SIG40 signal.
            bdr.add(GdbCmd::ConsoleExec("signal SIG40".to_string()));
        }
        let all_cmds = bdr.build().join("");
        self.write(all_cmds).await?;
        Ok(())
    }

    /// Sync the state of the session with the binary group if any.
    /// For example, existing breakpoints are automatically inserted.
    ///
    /// Note: When this function is called, the group manager should
    /// have the latest updates, a.k.a., the current session is added to
    /// the group.
    pub async fn sync_bkpts_state(&self) -> Result<()> {
        // TODO: Finished this function
        if let Some(grp_id) = get_group_mgr().get_grp_id_by_sid(self.sid) {
            // insert existing breakpoints
            let bkpts = get_bkpt_mgr().get_bkpts_by_grp_id(grp_id);
            for bkpt in &bkpts{
                let loc = bkpt.get_loc();
                debug!("Inserting existing breakpoint at location: {:?}", loc);
                let bkpt_path = loc.to_bkpt_path();
                let response = api::send_and_return(&format! {"-break-insert {}", bkpt_path})?
                    .to(Target::Session(self.sid))
                    .await
                    .context(format!(
                        "Failed to send insert breakpoint at location: {}",
                        bkpt_path
                    ))?;
                let bkpt_info = response
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
                get_bkpt_mgr().setup_grp_bkpt_for_new_session(
                    bkpt.get_id(),
                    grp_id,
                    self.sid,
                    local_bkpt_id,
                );
            }
        }
        Ok(())
    }

    /// Setup GDB logging configuration
    /// Creates log directory if it doesn't exist and configures GDB to log to
    /// /tmp/ddb/gdb_logs/<hostname>_<pid>_gdb.txt
    async fn setup_gdb_logging(&self, bdr: &mut DbgCmdListBuilder<GdbCmd>) -> Result<()> {
        use crate::common::utils::get_hostname;
        use std::fs;

        // Get the PID from the session config
        let pid = match &self.config.mode {
            DbgMode::REMOTE(DbgStartMode::ATTACH(pid)) => *pid,
            _ => return Err(anyhow::anyhow!("Cannot setup logging for non-attach mode")),
        };

        // Get hostname
        let hostname = get_hostname().context("Failed to get hostname for GDB logging")?;

        // Create log directory
        let log_dir = Path::new("/tmp/ddb/gdb_logs");
        fs::create_dir_all(log_dir).context("Failed to create GDB log directory")?;

        // Create log file path: /tmp/ddb/gdb_logs/<hostname>_<pid>_gdb.txt
        let log_file = log_dir.join(format!("{}_{}_gdb.txt", hostname, pid));

        debug!("Setting up GDB logging to: {:?}", log_file);

        // Set the logging file
        bdr.add(GdbCmd::SetOption(GdbOption::LoggingFile(
            log_file.to_string_lossy().to_string(),
        )));

        // Enable logging
        bdr.add(GdbCmd::SetOption(GdbOption::Logging(true)));

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

        // Configure GDB logging if enabled in config
        if Config::global().conf.gdb.logging {
            self.setup_gdb_logging(&mut bdr).await?;
        } else {
            bdr.add(GdbCmd::SetOption(GdbOption::Logging(false)));
        }

        bdr.add(GdbCmd::SetOption(GdbOption::MiAsync(true)));

        // source framework-specific GDB extension scripts
        match Config::global().framework {
            Framework::GRPC | Framework::Nu | Framework::Quicksand => {
                let gdb_ext_path = Path::new(DEFAULT_GDB_EXT_DIR).join(DEFAULT_GDB_EXT_NAME);
                bdr.add(GdbCmd::ConsoleExec(format!(
                    r#"source {}"#,
                    gdb_ext_path.to_str().unwrap()
                )));
            }
            _ => {}
        }
        match Config::global().framework {
            Framework::Nu | Framework::Quicksand => {
                if Config::global().conf.support_migration {
                    let gdb_ext_path = Path::new(DEFAULT_GDB_EXT_DIR).join(PROCLET_GDB_EXT_NAME);
                    bdr.add(GdbCmd::ConsoleExec(format!(
                        r#"source {}"#,
                        gdb_ext_path.to_str().unwrap()
                    )));
                }
            }
            _ => {}
        }

        // apply frame filter settings if any
        if let Some(frame_filter_cfg) = &Config::global().frame_filter {
            debug!("Applying frame filter settings: {:?}", frame_filter_cfg);
            let gdb_ext_frame_filter_path =
                Path::new(DEFAULT_GDB_EXT_DIR).join(DEFAULT_GDB_EXT_FRAME_FILTER_NAME);
            bdr.add(GdbCmd::ConsoleExec(format!(
                r#"source {}"#,
                gdb_ext_frame_filter_path.to_str().unwrap()
            )));

            bdr.add(GdbCmd::EnableFrameFilter);
            bdr.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::Enable));
            for pattern in &frame_filter_cfg.filter_file {
                let args: FrameFilterAddArgs = pattern.into();
                bdr.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::AddFile(args)));
            }
            for pattern in &frame_filter_cfg.filter_function {
                let args: FrameFilterAddArgs = pattern.into();
                bdr.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::AddFunction(args)));
            }
            for preset in &frame_filter_cfg.filter_preset {
                bdr.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::PresetEnable(
                    preset.clone(),
                )));
            }
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
        // - remove breakpoints associated with this session from the group breakpoint manager
        // - remove from router
        // - remove from the group manager
        // - remove from state manager
        // - shutdown the connection
        get_bkpt_mgr().clean_bkpts_for_terminated_session(self.sid);
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
