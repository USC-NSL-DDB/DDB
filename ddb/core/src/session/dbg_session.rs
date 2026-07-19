use anyhow::{Context, Result};
use bytes::Bytes;
use tracing::debug;

use super::{DbgMode, DbgSessionConfig, DbgStartMode};
use crate::cmd_flow::{api, get_router, session_runtime::SessionHandle, Target};
use crate::common;
use crate::common::Config;
use crate::dbg_ctrl::DebuggerTransportHandle;
use crate::debugger::get_debugger_backend;
use crate::plugin::get_framework_plugin;
use crate::session::lifecycle::SessionTerminationReporter;
#[cfg(not(feature = "lazy_source_map"))]
use crate::state::get_source_mgr;
use crate::state::{get_bkpt_mgr, get_group_mgr, get_state_mgr, STATES};

#[derive(Debug)]
pub struct DbgSession {
    pub sid: u64,
    pub config: DbgSessionConfig,
    transport: DebuggerTransportHandle,
    runtime: Option<SessionHandle>,
    runtime_task: Option<tokio::task::JoinHandle<()>>,
    cleanup_complete: bool,
}

impl DbgSession {
    pub fn new(config: DbgSessionConfig, transport: DebuggerTransportHandle) -> Self {
        let sid = crate::common::counter::next_session_id();
        Self {
            sid,
            config,
            transport,
            runtime: None,
            runtime_task: None,
            cleanup_complete: false,
        }
    }

    pub async fn start(
        &mut self,
        termination: SessionTerminationReporter,
    ) -> Result<SessionHandle> {
        STATES
            .register_session(
                self.sid,
                self.config
                    .tag
                    .clone()
                    .unwrap_or_else(|| format!("session-{}", self.sid))
                    .as_str(),
                self.config.service_meta.clone(),
            )
            .await;

        let handle = match self.launch_runtime(termination).await {
            Ok(handle) => handle,
            Err(error) => {
                if let Err(cleanup_error) = self.cleanup().await {
                    debug!(
                        sid = self.sid,
                        ?cleanup_error,
                        "failed to fully clean up a session that could not start"
                    );
                }
                return Err(error);
            }
        };

        if let Some(meta) = &self.config.service_meta {
            get_group_mgr().register_session(&meta.hash, meta.alias.clone(), self.sid);
        }
        get_router().add_session(handle.clone());

        #[cfg(not(feature = "lazy_source_map"))]
        {
            let sid = self.sid;
            tokio::spawn(async move {
                if let Err(error) = get_source_mgr().resolve_src_for(sid).await {
                    debug!("Failed to resolve source files: {:?}", error);
                }
            });
        }

        STATES.update_session_status_on(self.sid).await;
        Ok(handle)
    }

    async fn launch_runtime(
        &mut self,
        termination: SessionTerminationReporter,
    ) -> Result<SessionHandle> {
        let config = Config::global();
        let backend = get_debugger_backend();
        let plugin = get_framework_plugin();
        let plugin_bootstrap = plugin.debugger_bootstrap(config);
        let launch_command = backend.build_start_command(config.conf.sudo);

        let running = self.transport.launch(&launch_command).await?;
        let (handle, task) = SessionHandle::spawn(self.sid, running, termination);
        self.runtime = Some(handle.clone());
        self.runtime_task = Some(task);

        let bootstrap = match &self.config.mode {
            DbgMode::REMOTE(DbgStartMode::ATTACH(_)) | DbgMode::LOCAL(DbgStartMode::ATTACH(_)) => {
                backend.build_remote_attach_commands(
                    config,
                    &self.config,
                    plugin.as_ref(),
                    &plugin_bootstrap,
                )?
            }
            DbgMode::LOCAL(DbgStartMode::BINARY { .. }) => backend.build_local_binary_commands(
                config,
                &self.config,
                plugin.as_ref(),
                &plugin_bootstrap,
            )?,
            DbgMode::REMOTE(DbgStartMode::BINARY { .. }) => {
                anyhow::bail!("remote binary launch is not implemented")
            }
        }
        .join("");

        if !bootstrap.is_empty() {
            handle.write_raw(bootstrap).await?;
        }
        Ok(handle)
    }

    pub async fn post_start(&self) -> Result<()> {
        self.sync_bkpts_state().await?;

        let config = Config::global();
        let bootstrap = get_framework_plugin().debugger_bootstrap(config);
        let commands = bootstrap
            .post_start_commands
            .iter()
            .map(|command| command.render())
            .collect::<Vec<_>>()
            .join("");
        if !commands.is_empty() {
            self.write(commands).await?;
        }
        Ok(())
    }

    pub async fn sync_bkpts_state(&self) -> Result<()> {
        if let Some(group_id) = get_group_mgr().group_id_by_session(self.sid) {
            for breakpoint in get_bkpt_mgr().group_breakpoints(group_id) {
                let path = breakpoint.location().breakpoint_path();
                let response = api::command(&format!("-break-insert {}", path))?
                    .target(Target::Session(self.sid))
                    .state_consistent()
                    .execute()
                    .await
                    .with_context(|| format!("Failed to insert existing breakpoint at {}", path))?;
                let local_id = response
                    .get_responses()
                    .first()
                    .and_then(|response| response.get_payload())
                    .and_then(|payload| payload.get("bkpt"))
                    .and_then(|breakpoint| breakpoint.expect_dict_ref().ok())
                    .and_then(|breakpoint| breakpoint.get("number"))
                    .and_then(|number| number.expect_string_ref().ok())
                    .ok_or_else(|| anyhow::anyhow!("breakpoint response is missing number"))?
                    .parse::<u64>()?;
                get_bkpt_mgr()
                    .attach_group_breakpoint_session_target(
                        breakpoint.id(),
                        group_id,
                        self.sid,
                        local_id,
                    )
                    .await;
            }
        }
        Ok(())
    }

    pub async fn cleanup(&mut self) -> Result<()> {
        if self.cleanup_complete {
            return Ok(());
        }

        debug!("Cleaning up session with config: {:?}", self.config);
        get_state_mgr().update_session_status_off(self.sid).await;
        get_router().remove_session(self.sid);
        get_bkpt_mgr()
            .clean_bkpts_for_terminated_session(self.sid)
            .await;
        get_group_mgr().remove_session(self.sid);
        get_state_mgr().remove_session(self.sid).await;

        let mut first_error = None;
        if self.transport.is_open() && self.runtime.is_some() {
            let exit_policy = match &self.config.on_exit {
                common::config::OnExit::DETACH => "detach\n",
                common::config::OnExit::KILL => "kill\n",
            };
            if let Err(error) = self.write(exit_policy).await {
                first_error = Some(error.context("Failed to apply debugger exit policy"));
            }
            if let Err(error) = self.write("exit\n").await {
                if first_error.is_none() {
                    first_error = Some(error.context("Failed to exit debugger"));
                }
            }

            let mut retries = 0;
            while self.transport.is_open() && retries < 10 {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                retries += 1;
            }
        }

        if let Err(error) = self.transport.close().await {
            if first_error.is_none() {
                first_error = Some(error.context("Failed to close debugger transport"));
            }
        }

        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await;
        }
        if let Some(task) = self.runtime_task.take() {
            let _ = task.await;
        }

        self.cleanup_complete = true;
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn write(&self, command: impl Into<Bytes>) -> Result<()> {
        self.runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session runtime is not started"))?
            .write_raw(command)
            .await
    }
}
