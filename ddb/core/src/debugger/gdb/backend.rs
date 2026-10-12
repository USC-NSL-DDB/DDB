use std::path::Path;

use anyhow::{anyhow, Context, Result};
use tracing::debug;

use crate::{
    common::{
        config::{Config, OnExit},
        default_vals::{DEFAULT_GDB_EXT_DIR, DEFAULT_GDB_EXT_FRAME_FILTER_NAME},
        utils::get_hostname,
    },
    debugger::{
        protocol::DebuggerProtocol, BundledDebuggerAsset, DebuggerBackend, DebuggerBootstrapPlan,
        DebuggerCapabilities, DebuggerSessionContext, DebuggerSignal,
    },
    plugin::{FrameworkDebuggerBootstrap, FrameworkPlugin},
    session::{SessionMode, SessionRequest, SessionStart},
};

use super::{
    command::{
        DbgCmdGenerator, DbgCmdListBuilder, FrameFilterAddArgs, FrameFilterCmdArg, GdbCmd,
        GdbOption,
    },
    runtime::{
        gdb_start_cmd, CORE_GDB_RUNTIME_ASSET, FRAME_FILTER_GDB_RUNTIME_ASSET,
        PROCLET_GDB_RUNTIME_ASSET,
    },
};

#[derive(Debug, Default)]
pub struct GdbBackend;

impl GdbBackend {
    fn apply_common_setup(
        &self,
        config: &Config,
        session: &SessionRequest,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
        builder: &mut DbgCmdListBuilder<GdbCmd>,
    ) -> Result<()> {
        config.conf.debugger.validate()?;
        if config.conf.gdb.logging {
            match &session.mode {
                SessionMode::Remote(SessionStart::Attach(_)) => {
                    self.setup_logging_commands(session, builder)?;
                }
                _ => {
                    builder.add(GdbCmd::SetOption(GdbOption::Logging(false)));
                }
            }
        } else {
            builder.add(GdbCmd::SetOption(GdbOption::Logging(false)));
        }

        builder.add(GdbCmd::SetOption(GdbOption::MiAsync(true)));

        builder.add(GdbCmd::ConsoleExec(format!(
            "source {}",
            CORE_GDB_RUNTIME_ASSET.output_path().to_string_lossy()
        )));
        // GDB must retain the policy if DDB dies before sending shutdown commands.
        builder.add(GdbCmd::ConsoleExec(format!(
            "set ddb-on-exit {}",
            match session.on_exit {
                OnExit::KILL => "kill",
                OnExit::DETACH => "detach",
            }
        )));

        if plugin_bootstrap.requires_proclet_runtime {
            builder.add(GdbCmd::ConsoleExec(format!(
                "source {}",
                PROCLET_GDB_RUNTIME_ASSET.output_path().to_string_lossy()
            )));
        }
        for script in &plugin_bootstrap.scripts {
            builder.add(GdbCmd::ConsoleExec(format!(
                "source {}",
                script.to_string_lossy()
            )));
        }

        if let Some(frame_filter_cfg) = &config.frame_filter {
            debug!("Applying frame filter settings: {:?}", frame_filter_cfg);
            let frame_filter_script =
                Path::new(DEFAULT_GDB_EXT_DIR).join(DEFAULT_GDB_EXT_FRAME_FILTER_NAME);
            builder.add(GdbCmd::ConsoleExec(format!(
                "source {}",
                frame_filter_script.to_string_lossy()
            )));
            builder.add(GdbCmd::EnableFrameFilter);
            builder.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::Enable));
            for pattern in &frame_filter_cfg.filter_file {
                let args: FrameFilterAddArgs = pattern.into();
                builder.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::AddFile(args)));
            }
            for pattern in &frame_filter_cfg.filter_function {
                let args: FrameFilterAddArgs = pattern.into();
                builder.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::AddFunction(args)));
            }
            for preset in &frame_filter_cfg.filter_preset {
                builder.add(GdbCmd::FrameFilterCmd(FrameFilterCmdArg::PresetEnable(
                    preset.clone(),
                )));
            }
        }

        for cmd in &plugin_bootstrap.pre_attach_commands {
            builder.add(GdbCmd::ConsoleExec(cmd.command.trim().to_string()));
        }

        for cmd in &session.prerun_debugger_cmds {
            builder.add(GdbCmd::ConsoleExec(cmd.command.trim().to_string()));
        }

        if config.conf.debugger.pretty_printing {
            builder.add(GdbCmd::Plain("-enable-pretty-printing".into()));
        }
        for mapping in &config.conf.debugger.source_mappings {
            builder.add(GdbCmd::ConsoleExec(format!(
                "set substitute-path {} {}",
                serde_json::to_string(&mapping.from)?,
                serde_json::to_string(&mapping.to)?
            )));
        }

        Ok(())
    }

    fn append_session_setup(
        config: &Config,
        builder: &mut DbgCmdListBuilder<GdbCmd>,
    ) -> Result<()> {
        if let Some(function) = &config.conf.debugger.entry_function {
            builder.add(GdbCmd::Plain(format!(
                "-break-insert -t {}",
                serde_json::to_string(function)?
            )));
        }
        for command in &config.conf.debugger.gdb.commands {
            builder.add(GdbCmd::ConsoleExec(command.clone()));
        }
        Ok(())
    }

    fn setup_logging_commands(
        &self,
        session: &SessionRequest,
        builder: &mut DbgCmdListBuilder<GdbCmd>,
    ) -> Result<()> {
        use std::fs;

        let pid = match &session.mode {
            SessionMode::Remote(SessionStart::Attach(pid)) => *pid,
            _ => return Err(anyhow!("Cannot setup logging for non-attach mode")),
        };

        let hostname = get_hostname().context("Failed to get hostname for GDB logging")?;
        let log_dir = Path::new("/tmp/ddb/gdb_logs");
        fs::create_dir_all(log_dir).context("Failed to create GDB log directory")?;

        let log_file = log_dir.join(format!("{}_{}_gdb.txt", hostname, pid));
        debug!("Setting up GDB logging to: {:?}", log_file);
        builder.add(GdbCmd::SetOption(GdbOption::LoggingFile(
            log_file.to_string_lossy().to_string(),
        )));
        builder.add(GdbCmd::SetOption(GdbOption::Logging(true)));
        Ok(())
    }
}

impl DebuggerBackend for GdbBackend {
    fn name(&self) -> &'static str {
        "gdb"
    }

    fn capabilities(&self) -> DebuggerCapabilities {
        DebuggerCapabilities {
            proclet_migration: true,
            serviceweaver_remote_backtrace: true,
        }
    }

    fn create_protocol(&self, _context: &DebuggerSessionContext) -> Box<dyn DebuggerProtocol> {
        Box::new(super::protocol::GdbMiProtocol::default())
    }

    fn bundled_assets(&self, config: &Config) -> Vec<BundledDebuggerAsset> {
        let mut assets = vec![CORE_GDB_RUNTIME_ASSET, FRAME_FILTER_GDB_RUNTIME_ASSET];
        if config.handle_migration() {
            assets.push(PROCLET_GDB_RUNTIME_ASSET);
        }
        assets
    }

    fn build_start_command(&self, sudo: bool) -> String {
        gdb_start_cmd(sudo)
    }

    fn build_remote_attach_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        _plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
        _context: &DebuggerSessionContext,
    ) -> Result<DebuggerBootstrapPlan> {
        let mut builder = DbgCmdListBuilder::<GdbCmd>::new();
        self.apply_common_setup(config, session, plugin_bootstrap, &mut builder)?;

        match &session.mode {
            SessionMode::Remote(SessionStart::Attach(pid)) => {
                builder.add(GdbCmd::TargetAttach(*pid));
            }
            SessionMode::Local(SessionStart::Attach(pid)) => {
                builder.add(GdbCmd::TargetAttach(*pid));
            }
            _ => return Err(anyhow!("Invalid mode for remote attach")),
        }

        Self::append_session_setup(config, &mut builder)?;
        for cmd in &session.postrun_debugger_cmds {
            builder.add(GdbCmd::ConsoleExec(cmd.command.trim().to_string()));
        }

        Ok(DebuggerBootstrapPlan::commands(builder.build()))
    }

    fn build_local_binary_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        _plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
        _context: &DebuggerSessionContext,
    ) -> Result<DebuggerBootstrapPlan> {
        let mut builder = DbgCmdListBuilder::<GdbCmd>::new();
        self.apply_common_setup(config, session, plugin_bootstrap, &mut builder)?;

        match &session.mode {
            SessionMode::Local(SessionStart::Binary { path, args }) => {
                builder.add(GdbCmd::FileExecAndSym(path.clone()));
                if !args.is_empty() {
                    builder.add(GdbCmd::ExeArgs(args.join(" ")));
                }
            }
            _ => return Err(anyhow!("Invalid mode for local binary launch")),
        }

        Self::append_session_setup(config, &mut builder)?;
        for cmd in &session.postrun_debugger_cmds {
            builder.add(GdbCmd::ConsoleExec(cmd.command.trim().to_string()));
        }

        builder.add(GdbCmd::Plain(if session.stop_at_entry {
            "-exec-run --start".to_string()
        } else {
            "-exec-run".to_string()
        }));

        Ok(DebuggerBootstrapPlan::commands(builder.build()))
    }

    fn interrupt_command(&self) -> String {
        GdbCmd::Interrupt.generate()
    }

    fn console_exec_command(&self, command: &str) -> String {
        GdbCmd::ConsoleExec(command.to_string()).generate()
    }

    fn signal_command(&self, signal: &DebuggerSignal) -> Result<String> {
        Ok(self.console_exec_command(&format!("signal {}", signal.as_str())))
    }

    fn shutdown_commands(&self, on_exit: &OnExit) -> String {
        let policy = match on_exit {
            OnExit::DETACH => "detach\n",
            OnExit::KILL => "kill\n",
        };
        format!("{policy}exit\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dbg_ctrl::TransportSpec, plugin::resolve_framework_plugin, session::SessionRequestBuilder,
    };

    #[test]
    fn native_setup_runs_once_for_launch_and_attach_without_lldb_commands() {
        let config = Config::from_str("Conf:\n  Debugger:\n    entry_function: main\n    source_mappings: [{from: '/build with spaces', to: '/source with spaces'}]\n    gdb: {commands: ['set print elements 33']}\n    lldb: {commands: ['settings set target.max-children-count 33']}\n").unwrap();
        let plugin = resolve_framework_plugin(&config);
        let context = DebuggerSessionContext::for_test("0123456789abcdef0123456789abcdef");
        for launch in [true, false] {
            let mode = if launch {
                SessionMode::Local(SessionStart::Binary {
                    path: "/tmp/app".into(),
                    args: vec![],
                })
            } else {
                SessionMode::Local(SessionStart::Attach(42))
            };
            let request = SessionRequestBuilder::from_config(&config)
                .mode(mode)
                .transport(TransportSpec::Local)
                .build()
                .unwrap();
            let plan = if launch {
                GdbBackend.build_local_binary_commands(
                    &config,
                    &request,
                    plugin.as_ref(),
                    &plugin.debugger_bootstrap(&config),
                    &context,
                )
            } else {
                GdbBackend.build_remote_attach_commands(
                    &config,
                    &request,
                    plugin.as_ref(),
                    &plugin.debugger_bootstrap(&config),
                    &context,
                )
            }
            .unwrap();
            let index = |needle: &str| {
                plan.commands
                    .iter()
                    .position(|command| command.contains(needle))
                    .unwrap()
            };
            assert!(
                index(if launch {
                    "-file-exec-and-symbols"
                } else {
                    "-target-attach"
                }) < index("-break-insert -t")
            );
            assert!(index("-break-insert -t") < index("set print elements"));
            assert_eq!(
                plan.commands
                    .iter()
                    .filter(|command| command.contains("set print elements"))
                    .count(),
                1
            );
            assert!(plan
                .commands
                .iter()
                .any(|command| command.contains("-enable-pretty-printing")));
            assert!(plan
                .commands
                .iter()
                .any(|command| command.contains("set substitute-path")));
            assert!(!plan
                .commands
                .iter()
                .any(|command| command.contains("max-children-count")));
        }
    }
}
