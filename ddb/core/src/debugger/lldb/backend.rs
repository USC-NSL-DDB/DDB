use anyhow::{anyhow, Context, Result};

use crate::{
    common::config::{Config, OnExit},
    debugger::{protocol::DebuggerProtocol, BundledDebuggerAsset, DebuggerBackend},
    plugin::{DebuggerBootstrapAction, FrameworkDebuggerBootstrap, FrameworkPlugin},
    session::{SessionMode, SessionRequest, SessionStart},
};

use super::{
    protocol::{encode_request, LldbJsonProtocol},
    runtime::{lldb_start_command, LLDB_BRIDGE_ASSET},
};

#[derive(Debug, Default)]
pub struct LldbBackend;

impl LldbBackend {
    fn bridge_command(command: &str) -> Result<String> {
        let bytes = encode_request(0, command, None)?;
        String::from_utf8(bytes.to_vec()).context("LLDB bridge request must be UTF-8")
    }

    fn console_command(command: &str) -> Result<String> {
        Self::bridge_command(&format!(
            "-interpreter-exec console {}",
            serde_json::to_string(command)?
        ))
    }

    fn apply_common_setup(
        &self,
        config: &Config,
        session: &SessionRequest,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
        commands: &mut Vec<String>,
    ) -> Result<()> {
        self.validate_config(config)?;

        commands.push("settings set auto-confirm true\n".to_string());
        commands.push(format!(
            "command script import {}\n",
            LLDB_BRIDGE_ASSET.output_path().to_string_lossy()
        ));
        commands.push("script ddb_lldb_bridge.run(lldb.debugger)\n".to_string());
        commands.push(Self::bridge_command(&format!(
            "-ddb-set-stack-prewarm {}",
            config.conf.debugger.eager_stack_warmup
        ))?);

        for script in &plugin_bootstrap.scripts {
            commands.push(Self::console_command(&format!(
                "command script import {}",
                script.to_string_lossy()
            ))?);
        }

        if let Some(frame_filter) = &config.frame_filter {
            commands.push(Self::bridge_command("-ddb-filter-config --enable")?);
            for pattern in &frame_filter.filter_file {
                commands.push(Self::bridge_command(&format!(
                    "-ddb-filter-config --add-file {} --match-type {}",
                    serde_json::to_string(&pattern.pattern)?,
                    pattern.match_type.as_str()
                ))?);
            }
            for pattern in &frame_filter.filter_function {
                commands.push(Self::bridge_command(&format!(
                    "-ddb-filter-config --add-function {} --match-type {}",
                    serde_json::to_string(&pattern.pattern)?,
                    pattern.match_type.as_str()
                ))?);
            }
            for preset in &frame_filter.filter_preset {
                commands.push(Self::bridge_command(&format!(
                    "-ddb-filter-config --preset-enable {}",
                    serde_json::to_string(preset)?
                ))?);
            }
        }

        for command in &plugin_bootstrap.pre_attach_commands {
            commands.push(Self::bridge_command(command.command.trim())?);
        }
        for command in &session.prerun_debugger_cmds {
            commands.push(Self::bridge_command(command.command.trim())?);
        }
        Ok(())
    }

    fn append_postrun(session: &SessionRequest, commands: &mut Vec<String>) -> Result<()> {
        for command in &session.postrun_debugger_cmds {
            commands.push(Self::bridge_command(command.command.trim())?);
        }
        Ok(())
    }
}

impl DebuggerBackend for LldbBackend {
    fn name(&self) -> &'static str {
        "lldb"
    }

    fn create_protocol(&self) -> Box<dyn DebuggerProtocol> {
        Box::new(LldbJsonProtocol::default())
    }

    fn bundled_assets(&self, _config: &Config) -> Vec<BundledDebuggerAsset> {
        vec![LLDB_BRIDGE_ASSET]
    }

    fn build_start_command(&self, sudo: bool) -> String {
        lldb_start_command(sudo)
    }

    fn build_remote_attach_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        _plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
    ) -> Result<Vec<String>> {
        let mut commands = Vec::new();
        self.apply_common_setup(config, session, plugin_bootstrap, &mut commands)?;
        let pid = match &session.mode {
            SessionMode::Remote(SessionStart::Attach(pid))
            | SessionMode::Local(SessionStart::Attach(pid)) => *pid,
            _ => return Err(anyhow!("invalid mode for LLDB attach")),
        };
        commands.push(Self::bridge_command(&format!("-target-attach {pid}"))?);
        Self::append_postrun(session, &mut commands)?;
        Ok(commands)
    }

    fn build_local_binary_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        _plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
    ) -> Result<Vec<String>> {
        let mut commands = Vec::new();
        self.apply_common_setup(config, session, plugin_bootstrap, &mut commands)?;
        let (path, args) = match &session.mode {
            SessionMode::Local(SessionStart::Binary { path, args }) => (path, args),
            _ => return Err(anyhow!("invalid mode for LLDB binary launch")),
        };
        commands.push(Self::bridge_command(&format!(
            "-file-exec-and-symbols {}",
            serde_json::to_string(path)?
        ))?);
        if !args.is_empty() {
            let args = args
                .iter()
                .map(serde_json::to_string)
                .collect::<serde_json::Result<Vec<_>>>()?
                .join(" ");
            commands.push(Self::bridge_command(&format!("-exec-arguments {args}"))?);
        }
        Self::append_postrun(session, &mut commands)?;
        commands.push(Self::bridge_command(if session.stop_at_entry {
            "-exec-run --start"
        } else {
            "-exec-run"
        })?);
        Ok(commands)
    }

    fn interrupt_command(&self) -> String {
        "-exec-interrupt-if-running".to_string()
    }

    fn console_exec_command(&self, command: &str) -> String {
        format!(
            "-interpreter-exec console {}",
            serde_json::to_string(command).expect("serializing a string cannot fail")
        )
    }

    fn bootstrap_action_command(&self, action: &DebuggerBootstrapAction) -> String {
        match action {
            DebuggerBootstrapAction::Signal(signal) => Self::bridge_command(&format!(
                "-interpreter-exec console {}",
                serde_json::to_string(&format!("process signal {signal}"))
                    .expect("serializing a string cannot fail")
            ))
            .expect("serializing an LLDB bootstrap request cannot fail"),
        }
    }

    fn shutdown_commands(&self, on_exit: &OnExit) -> String {
        let policy = match on_exit {
            OnExit::DETACH => "detach",
            OnExit::KILL => "kill",
        };
        Self::bridge_command(&format!("-ddb-shutdown {policy}"))
            .expect("serializing an LLDB shutdown request cannot fail")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        common::config::StaticSessionConfig, dbg_ctrl::TransportSpec,
        plugin::resolve_framework_plugin, session::SessionRequestBuilder, state::ServiceIdentity,
    };

    fn binary_request(config: &Config) -> SessionRequest {
        let session = StaticSessionConfig {
            tag: "fixture".to_string(),
            hash: "fixture".to_string(),
            alias: "fixture".to_string(),
            binary_path: "/tmp/program with spaces".to_string(),
            binary_args: vec!["--name".to_string(), "two words".to_string()],
            ..StaticSessionConfig::default()
        };
        SessionRequestBuilder::from_config(config)
            .tag(session.tag)
            .service_identity(ServiceIdentity::new(session.hash, session.alias))
            .mode(SessionMode::Local(SessionStart::Binary {
                path: session.binary_path,
                args: session.binary_args,
            }))
            .transport(TransportSpec::Local)
            .build()
            .unwrap()
    }

    #[test]
    fn local_launch_bootstrap_enters_bridge_before_json_requests() {
        let config = Config::default();
        let plugin = resolve_framework_plugin(&config);
        let request = binary_request(&config);
        let commands = LldbBackend
            .build_local_binary_commands(
                &config,
                &request,
                plugin.as_ref(),
                &plugin.debugger_bootstrap(&config),
            )
            .unwrap();

        assert_eq!(commands[0], "settings set auto-confirm true\n");
        assert!(commands[1].starts_with("command script import "));
        assert_eq!(commands[2], "script ddb_lldb_bridge.run(lldb.debugger)\n");
        assert!(commands[3].contains("-ddb-set-stack-prewarm true"));
        assert!(commands
            .iter()
            .any(|command| command.contains("-file-exec-and-symbols")));
        assert!(commands
            .iter()
            .any(|command| command.contains("program with spaces")));
    }

    #[test]
    fn shutdown_returns_control_to_lldb_driver() {
        let commands = LldbBackend.shutdown_commands(&OnExit::DETACH);
        assert!(commands.contains("-ddb-shutdown detach"));
        assert!(!commands.contains("quit"));
    }
}
