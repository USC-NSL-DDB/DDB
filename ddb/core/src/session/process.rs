use anyhow::Result;
use bytes::Bytes;
use tracing::debug;

use super::{SessionMode, SessionRequest, SessionStart};
use crate::{
    cmd_flow::session_runtime::SessionHandle,
    common::{self, Config},
    dbg_ctrl::DebuggerTransportHandle,
    debugger::get_debugger_backend,
    plugin::get_framework_plugin,
    session::lifecycle::SessionTerminationReporter,
};

/// Owns the debugger transport and its command runtime for one session.
#[derive(Debug)]
pub struct SessionProcess {
    sid: u64,
    request: SessionRequest,
    transport: DebuggerTransportHandle,
    runtime: Option<SessionHandle>,
    runtime_task: Option<tokio::task::JoinHandle<()>>,
    shutdown_complete: bool,
}

impl SessionProcess {
    pub fn new(request: SessionRequest, transport: DebuggerTransportHandle) -> Self {
        Self {
            sid: crate::common::counter::next_session_id(),
            request,
            transport,
            runtime: None,
            runtime_task: None,
            shutdown_complete: false,
        }
    }

    pub fn sid(&self) -> u64 {
        self.sid
    }

    pub fn request(&self) -> &SessionRequest {
        &self.request
    }

    pub async fn launch(
        &mut self,
        termination: SessionTerminationReporter,
    ) -> Result<SessionHandle> {
        let config = Config::global();
        let backend = get_debugger_backend();
        let plugin = get_framework_plugin();
        let plugin_bootstrap = plugin.debugger_bootstrap(config);
        let launch_command = backend.build_start_command(self.request.sudo);

        let running = self.transport.launch(&launch_command).await?;
        let (handle, task) = SessionHandle::spawn(self.sid, running, termination);
        self.runtime = Some(handle.clone());
        self.runtime_task = Some(task);

        let bootstrap = match &self.request.mode {
            SessionMode::Remote(SessionStart::Attach(_))
            | SessionMode::Local(SessionStart::Attach(_)) => backend.build_remote_attach_commands(
                config,
                &self.request,
                plugin.as_ref(),
                &plugin_bootstrap,
            )?,
            SessionMode::Local(SessionStart::Binary { .. }) => backend
                .build_local_binary_commands(
                    config,
                    &self.request,
                    plugin.as_ref(),
                    &plugin_bootstrap,
                )?,
            SessionMode::Remote(SessionStart::Binary { .. }) => {
                anyhow::bail!("remote binary launch is not implemented")
            }
        }
        .join("");

        if !bootstrap.is_empty() {
            handle.write_raw(bootstrap).await?;
        }
        Ok(handle)
    }

    pub async fn finish_bootstrap(&self) -> Result<()> {
        let commands = get_framework_plugin()
            .debugger_bootstrap(Config::global())
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

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.shutdown_complete {
            return Ok(());
        }

        debug!(sid = self.sid, "shutting down debugger session process");
        let mut first_error = None;
        if self.transport.is_open() && self.runtime.is_some() {
            let exit_policy = match &self.request.on_exit {
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

        self.shutdown_complete = true;
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
