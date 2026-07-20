use std::path::Path;

use anyhow::{anyhow, Context, Result};
use tracing::debug;

use crate::{
    common::{
        config::Config,
        default_vals::{DEFAULT_GDB_EXT_DIR, DEFAULT_GDB_EXT_FRAME_FILTER_NAME},
        utils::get_hostname,
    },
    dbg_cmd::{DbgCmdGenerator, DbgCmdListBuilder},
    debugger::{BundledDebuggerAsset, DebuggerBackend},
    plugin::{FrameworkDebuggerBootstrap, FrameworkPlugin},
    session::{SessionMode, SessionRequest, SessionStart},
};

use super::{
    command::{FrameFilterAddArgs, FrameFilterCmdArg, GdbCmd, GdbOption},
    runtime::{gdb_start_cmd, CORE_GDB_RUNTIME_ASSET, FRAME_FILTER_GDB_RUNTIME_ASSET},
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
            builder.add(cmd);
        }

        for cmd in &session.prerun_debugger_cmds {
            builder.add(cmd);
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

    fn bundled_assets(&self, _config: &Config) -> Vec<BundledDebuggerAsset> {
        vec![CORE_GDB_RUNTIME_ASSET, FRAME_FILTER_GDB_RUNTIME_ASSET]
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
    ) -> Result<Vec<String>> {
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

        for cmd in &session.postrun_debugger_cmds {
            builder.add(cmd);
        }

        Ok(builder.build())
    }

    fn build_local_binary_commands(
        &self,
        config: &Config,
        session: &SessionRequest,
        _plugin: &dyn FrameworkPlugin,
        plugin_bootstrap: &FrameworkDebuggerBootstrap,
    ) -> Result<Vec<String>> {
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

        for cmd in &session.postrun_debugger_cmds {
            builder.add(cmd);
        }

        builder.add(GdbCmd::Plain(if session.stop_at_entry {
            "-exec-run --start".to_string()
        } else {
            "-exec-run".to_string()
        }));

        Ok(builder.build())
    }

    fn interrupt_command(&self) -> String {
        GdbCmd::Interrupt.generate()
    }

    fn console_exec_command(&self, command: &str) -> String {
        GdbCmd::ConsoleExec(command.to_string()).generate()
    }
}
