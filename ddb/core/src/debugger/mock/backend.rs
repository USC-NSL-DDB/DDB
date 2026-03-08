use anyhow::Result;

use crate::{
    common::config::Config,
    debugger::{BundledDebuggerAsset, DebuggerBackend},
    plugin::{FrameworkDebuggerBootstrap, FrameworkPlugin},
    session::DbgSessionConfig,
};

#[derive(Debug, Default)]
pub struct MockBackend;

impl DebuggerBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn bundled_assets(&self, _config: &Config) -> Vec<BundledDebuggerAsset> {
        Vec::new()
    }

    fn build_start_command(&self, _sudo: bool) -> String {
        "mock-debugger".to_string()
    }

    fn build_remote_attach_commands(
        &self,
        _config: &Config,
        session: &DbgSessionConfig,
        _plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
    ) -> Result<Vec<String>> {
        let mut commands = Vec::new();

        for script in &plugin_bootstrap.scripts {
            commands.push(format!(
                "-interpreter-exec console \"source {}\"\n",
                script.to_string_lossy()
            ));
        }

        for cmd in &plugin_bootstrap.pre_attach_commands {
            commands.push(cmd.render());
        }

        for cmd in &session.prerun_debugger_cmds {
            commands.push(cmd.render());
        }

        commands.push("-mock-bootstrap\n".to_string());

        for cmd in &session.postrun_debugger_cmds {
            commands.push(cmd.render());
        }

        Ok(commands)
    }

    fn interrupt_command(&self) -> String {
        "-exec-interrupt-if-running".to_string()
    }

    fn console_exec_command(&self, command: &str) -> String {
        format!("-interpreter-exec console \"{}\"", command)
    }
}
