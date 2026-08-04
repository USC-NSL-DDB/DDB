use anyhow::{Context, Result};
use bytes::Bytes;
use std::sync::Arc;
use tracing::debug;

use super::{SessionMode, SessionRequest, SessionStart};
use crate::{
    cmd_flow::{
        event::DebuggerEventReducer,
        session_runtime::{CompletionConsistency, SessionCommand, SessionHandle, COMMAND_TIMEOUT},
    },
    common::Config,
    dbg_ctrl::DebuggerTransportHandle,
    debugger::{DebuggerBackend, DebuggerSessionContext},
    plugin::FrameworkPlugin,
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
    config: Arc<Config>,
    backend: Arc<dyn DebuggerBackend>,
    plugin: Arc<dyn FrameworkPlugin>,
    reducer: Arc<DebuggerEventReducer>,
}

impl SessionProcess {
    pub fn new(
        sid: u64,
        request: SessionRequest,
        transport: DebuggerTransportHandle,
        config: Arc<Config>,
        backend: Arc<dyn DebuggerBackend>,
        plugin: Arc<dyn FrameworkPlugin>,
        reducer: Arc<DebuggerEventReducer>,
    ) -> Self {
        Self {
            sid,
            request,
            transport,
            runtime: None,
            runtime_task: None,
            shutdown_complete: false,
            config,
            backend,
            plugin,
            reducer,
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
        let config = self.config.as_ref();
        let backend = self.backend.as_ref();
        let plugin = self.plugin.as_ref();
        let plugin_bootstrap = plugin.debugger_bootstrap(config);
        let launch_command = backend.build_start_command(self.request.sudo);
        let context = DebuggerSessionContext::new();

        let running = self.transport.launch(&launch_command).await?;
        let (handle, task) = SessionHandle::spawn(
            self.sid,
            running,
            self.backend.create_protocol(&context),
            termination,
            Arc::clone(&self.reducer),
        );
        self.runtime = Some(handle.clone());
        self.runtime_task = Some(task);

        let bootstrap = match &self.request.mode {
            SessionMode::Remote(SessionStart::Attach(_))
            | SessionMode::Local(SessionStart::Attach(_)) => backend.build_remote_attach_commands(
                config,
                &self.request,
                plugin,
                &plugin_bootstrap,
                &context,
            )?,
            SessionMode::Local(SessionStart::Binary { .. }) => backend
                .build_local_binary_commands(
                    config,
                    &self.request,
                    plugin,
                    &plugin_bootstrap,
                    &context,
                )?,
            SessionMode::Remote(SessionStart::Binary { .. }) => {
                anyhow::bail!("remote binary launch is not implemented")
            }
        };

        if !bootstrap.protocol_prelude.is_empty() {
            handle.write_raw(bootstrap.protocol_prelude).await?;
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, handle.wait_until_ready()).await {
            Ok(result) => result.context("debugger protocol failed during bootstrap")?,
            Err(_) => anyhow::bail!(
                "debugger protocol did not become ready within {:?}",
                COMMAND_TIMEOUT
            ),
        }
        for command in bootstrap.commands {
            Self::execute_bootstrap_command(&handle, command).await?;
        }
        Ok(handle)
    }

    pub async fn finish_bootstrap(&self) -> Result<()> {
        let actions = self
            .plugin
            .debugger_bootstrap(self.config.as_ref())
            .post_start_actions;
        let handle = self
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session runtime is not started"))?;
        for action in &actions {
            Self::execute_bootstrap_command(handle, self.backend.bootstrap_action_command(action))
                .await?;
        }
        Ok(())
    }

    async fn execute_bootstrap_command(handle: &SessionHandle, command: String) -> Result<()> {
        let response = handle
            .execute(SessionCommand {
                command: command.clone(),
                thread_id: None,
                consistency: CompletionConsistency::StateConsistent,
            })
            .await
            .with_context(|| format!("debugger bootstrap command failed: {}", command.trim()))?;
        if response.get_message() == "error" {
            let detail = response
                .get_payload()
                .and_then(|payload| payload.get("msg"))
                .and_then(|message| message.expect_string_ref().ok())
                .unwrap_or("debugger returned an error without a message");
            anyhow::bail!(
                "debugger rejected bootstrap command '{}': {}",
                command.trim(),
                detail
            );
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
            let shutdown_commands = self.backend.shutdown_commands(&self.request.on_exit);
            if let Err(error) = self.write(shutdown_commands).await {
                first_error = Some(error.context("Failed to shut down debugger"));
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

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use async_trait::async_trait;
    use bytes::Bytes;

    use super::*;
    use crate::{
        cmd_flow::breakpoint::BreakpointEventPublisher,
        common::config::OnExit,
        connection::{RunningTransport, TransportEvent, TransportRequest},
        dbg_ctrl::{DebuggerTransport, TransportSpec},
        debugger::{
            protocol::DebuggerProtocol, BundledDebuggerAsset, DebuggerBootstrapPlan,
            DebuggerCapabilities, DebuggerSessionContext,
        },
        notification::NotificationManager,
        plugin::{DebuggerBootstrapAction, FrameworkDebuggerBootstrap, FrameworkPlugin},
        session::{lifecycle, SessionRequestBuilder},
        state::RuntimeModel,
    };

    #[derive(Debug)]
    struct FailingBootstrapBackend;

    impl DebuggerBackend for FailingBootstrapBackend {
        fn name(&self) -> &'static str {
            "failing-bootstrap"
        }

        fn capabilities(&self) -> DebuggerCapabilities {
            DebuggerCapabilities::default()
        }

        fn create_protocol(&self, _context: &DebuggerSessionContext) -> Box<dyn DebuggerProtocol> {
            Box::new(crate::debugger::gdb::protocol::GdbMiProtocol::default())
        }

        fn bundled_assets(&self, _config: &Config) -> Vec<BundledDebuggerAsset> {
            Vec::new()
        }

        fn build_start_command(&self, _sudo: bool) -> String {
            "failing-debugger".to_string()
        }

        fn build_remote_attach_commands(
            &self,
            _config: &Config,
            _session: &SessionRequest,
            _plugin: &dyn FrameworkPlugin,
            _plugin_bootstrap: &FrameworkDebuggerBootstrap,
            _context: &DebuggerSessionContext,
        ) -> Result<DebuggerBootstrapPlan> {
            Ok(DebuggerBootstrapPlan::commands(vec![
                "-bootstrap-fail\n".to_string()
            ]))
        }

        fn build_local_binary_commands(
            &self,
            config: &Config,
            session: &SessionRequest,
            plugin: &dyn FrameworkPlugin,
            plugin_bootstrap: &FrameworkDebuggerBootstrap,
            context: &DebuggerSessionContext,
        ) -> Result<DebuggerBootstrapPlan> {
            self.build_remote_attach_commands(config, session, plugin, plugin_bootstrap, context)
        }

        fn interrupt_command(&self) -> String {
            "-exec-interrupt".to_string()
        }

        fn console_exec_command(&self, command: &str) -> String {
            command.to_string()
        }

        fn bootstrap_action_command(&self, action: &DebuggerBootstrapAction) -> String {
            match action {
                DebuggerBootstrapAction::Signal(signal) => signal.clone(),
            }
        }

        fn shutdown_commands(&self, _on_exit: &OnExit) -> String {
            String::new()
        }
    }

    #[derive(Debug)]
    struct FailingBootstrapTransport {
        open: Arc<AtomicBool>,
        task: Option<tokio::task::JoinHandle<()>>,
    }

    impl FailingBootstrapTransport {
        fn new() -> Self {
            Self {
                open: Arc::new(AtomicBool::new(false)),
                task: None,
            }
        }
    }

    #[async_trait]
    impl DebuggerTransport for FailingBootstrapTransport {
        async fn launch(&mut self, _cmd: &str) -> Result<RunningTransport> {
            let (requests, request_rx) = flume::bounded(4);
            let (event_tx, events) = flume::bounded(4);
            self.open.store(true, Ordering::Release);
            let open = Arc::clone(&self.open);
            self.task = Some(tokio::spawn(async move {
                if let Ok(TransportRequest::Write { data, written }) = request_rx.recv_async().await
                {
                    let text = String::from_utf8_lossy(&data);
                    let token = text
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>();
                    let _ = written.send(Ok(()));
                    let _ = event_tx
                        .send_async(TransportEvent::Stdout(Bytes::from(format!(
                            "{token}^error,msg=\"bootstrap rejected\"\n"
                        ))))
                        .await;
                }
                open.store(false, Ordering::Release);
            }));
            Ok(RunningTransport::new(requests, events))
        }

        fn is_open(&self) -> bool {
            self.open.load(Ordering::Acquire)
        }

        async fn close(&mut self) -> Result<()> {
            self.open.store(false, Ordering::Release);
            if let Some(task) = self.task.take() {
                let _ = task.await;
            }
            Ok(())
        }
    }

    fn test_reducer() -> Arc<DebuggerEventReducer> {
        DebuggerEventReducer::new(
            RuntimeModel::new(),
            BreakpointEventPublisher::new(
                Arc::new(NotificationManager::new()),
                crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
            ),
        )
    }

    #[tokio::test]
    async fn bootstrap_error_response_fails_session_launch() {
        let config = Arc::new(Config::default());
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        let request = SessionRequestBuilder::from_config(config.as_ref())
            .mode(SessionMode::Local(SessionStart::Attach(42)))
            .transport(TransportSpec::Local)
            .build()
            .unwrap();
        let mut process = SessionProcess::new(
            1,
            request,
            Box::new(FailingBootstrapTransport::new()),
            config,
            Arc::new(FailingBootstrapBackend),
            plugin,
            test_reducer(),
        );
        let (lifecycle, _terminations) = lifecycle::channel();

        let result = process.launch(lifecycle.bind(1)).await;
        if result.is_ok() {
            process.shutdown().await.unwrap();
        }

        let error = result.expect_err("bootstrap error should reject session launch");
        assert!(error.to_string().contains("bootstrap rejected"));
    }
}
