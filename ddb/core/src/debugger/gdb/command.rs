use crate::common::config::{DebuggerCommand, FrameFilterMatchType, FrameFilterPatternConfig};
use std::fmt;

pub trait DbgCmdGenerator {
    fn generate(&self) -> String;
}

pub struct DbgCmdListBuilder<T>
where
    T: DbgCmdGenerator,
{
    cmds: Vec<T>,
}

impl<T> DbgCmdListBuilder<T>
where
    T: DbgCmdGenerator,
{
    pub fn new() -> Self {
        Self { cmds: Vec::new() }
    }

    pub fn add<U: Into<T>>(&mut self, cmd: U) -> &Self {
        self.cmds.push(cmd.into());
        self
    }

    pub fn build(self) -> Vec<String> {
        self.cmds.iter().map(DbgCmdGenerator::generate).collect()
    }
}
#[derive(Debug, Clone)]
pub struct FrameFilterAddArgs {
    pattern: String,
    match_type: FrameFilterMatchType,
}

impl FrameFilterAddArgs {
    pub fn new(pattern: &str, match_type: FrameFilterMatchType) -> Self {
        Self {
            pattern: pattern.to_string(),
            match_type,
        }
    }
}
impl fmt::Display for FrameFilterAddArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} --match-type {}",
            self.pattern,
            self.match_type.as_str()
        )
    }
}

impl From<FrameFilterPatternConfig> for FrameFilterAddArgs {
    fn from(config: FrameFilterPatternConfig) -> Self {
        FrameFilterAddArgs::new(&config.pattern, config.match_type)
    }
}

impl From<&FrameFilterPatternConfig> for FrameFilterAddArgs {
    fn from(config: &FrameFilterPatternConfig) -> Self {
        FrameFilterAddArgs::new(&config.pattern, config.match_type.clone())
    }
}

#[derive(Debug, Clone)]
pub enum FrameFilterCmdArg {
    Enable,
    AddFunction(FrameFilterAddArgs),
    AddFile(FrameFilterAddArgs),
    PresetEnable(String),
}

impl FrameFilterCmdArg {
    pub fn to_str(&self) -> String {
        match self {
            FrameFilterCmdArg::Enable => "--enable".to_string(),
            FrameFilterCmdArg::AddFunction(args) => format!("--add-function {args}"),
            FrameFilterCmdArg::AddFile(args) => format!("--add-file {args}"),
            FrameFilterCmdArg::PresetEnable(name) => format!("--preset-enable {}", name),
        }
    }
}

const DDB_FILTER_CONFIG_CMD: &str = "-ddb-filter-config";

#[derive(Debug, Clone)]
pub enum GdbCmd {
    SetOption(GdbOption),
    ConsoleExec(String),
    TargetAttach(u64),
    FileExecAndSym(String),
    ExeArgs(String),
    Plain(String),
    FrameFilterCmd(FrameFilterCmdArg),
    EnableFrameFilter,
    Interrupt,
}

impl From<DebuggerCommand> for GdbCmd {
    fn from(cmd: DebuggerCommand) -> Self {
        GdbCmd::Plain(cmd.command)
    }
}

impl From<&DebuggerCommand> for GdbCmd {
    fn from(cmd: &DebuggerCommand) -> Self {
        GdbCmd::Plain(cmd.command.clone())
    }
}

impl From<GdbCmd> for DebuggerCommand {
    fn from(cmd: GdbCmd) -> Self {
        let command = cmd.generate();
        DebuggerCommand {
            name: "unnamed cmd".to_string(),
            command,
        }
    }
}

impl DbgCmdGenerator for GdbCmd {
    fn generate(&self) -> String {
        let cmd = match self {
            GdbCmd::SetOption(opt) => format!("-gdb-set {}", opt.generate()),
            GdbCmd::ConsoleExec(cmd) => format!(r#"-interpreter-exec console "{}""#, cmd),
            GdbCmd::TargetAttach(pid) => format!("-target-attach {}", pid),
            GdbCmd::FileExecAndSym(bin_path) => format!("-file-exec-and-symbols {}", bin_path),
            GdbCmd::ExeArgs(args) => format!("-exec-arguments {}", args),
            GdbCmd::Plain(cmd) => cmd.clone(),
            GdbCmd::FrameFilterCmd(ff_cmd) => {
                format!("{} {}", DDB_FILTER_CONFIG_CMD, ff_cmd.to_str())
            }
            GdbCmd::EnableFrameFilter => "-enable-frame-filters".to_string(),
            GdbCmd::Interrupt => "-exec-interrupt".to_string(),
        };

        let cmd = cmd.trim().to_string();
        if !cmd.ends_with('\n') {
            cmd + "\n"
        } else {
            cmd
        }
    }
}

#[derive(Debug, Clone)]
pub enum GdbOption {
    LoggingFile(String),
    Logging(bool),
    MiAsync(bool),
}

impl DbgCmdGenerator for GdbOption {
    fn generate(&self) -> String {
        match self {
            GdbOption::LoggingFile(file_path) => {
                format!("logging file {}", file_path)
            }
            GdbOption::Logging(enable) => {
                format!("logging enabled {}", if *enable { "on" } else { "off" })
            }
            GdbOption::MiAsync(enable) => {
                format!("mi-async {}", if *enable { "on" } else { "off" })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gdb_cmd_generate() {
        let cmd = GdbCmd::SetOption(GdbOption::Logging(true));
        assert_eq!(cmd.generate(), "-gdb-set logging enabled on\n");

        let cmd = GdbCmd::ConsoleExec("info registers".to_string());
        assert_eq!(
            cmd.generate(),
            r#"-interpreter-exec console "info registers""#.to_string() + "\n"
        );

        let cmd = GdbCmd::TargetAttach(1234);
        assert_eq!(cmd.generate(), "-target-attach 1234\n");

        let cmd = GdbCmd::FileExecAndSym("/path/to/bin".to_string());
        assert_eq!(cmd.generate(), "-file-exec-and-symbols /path/to/bin\n");

        let cmd = GdbCmd::ExeArgs("arg1 arg2".to_string());
        assert_eq!(cmd.generate(), "-exec-arguments arg1 arg2\n");

        let cmd = GdbCmd::Plain("target remote localhost:1234".to_string());
        assert_eq!(cmd.generate(), "target remote localhost:1234\n");
    }

    #[test]
    fn test_gdb_cmd_builder() {
        let mut cmd_bdr = DbgCmdListBuilder::<GdbCmd>::new();
        cmd_bdr.add(GdbCmd::SetOption(GdbOption::Logging(true)));
        cmd_bdr.add(GdbCmd::SetOption(GdbOption::MiAsync(true)));
        cmd_bdr.add(GdbCmd::ConsoleExec("info registers".to_string()));
        cmd_bdr.add(GdbCmd::TargetAttach(1234));
        cmd_bdr.add(GdbCmd::FileExecAndSym("/path/to/bin".to_string()));
        cmd_bdr.add(GdbCmd::ExeArgs("arg1 arg2".to_string()));
        cmd_bdr.add(GdbCmd::Plain("target remote localhost:1234".to_string()));

        let cmds = cmd_bdr.build();
        assert_eq!(cmds[0].trim(), "-gdb-set logging enabled on");
        assert_eq!(cmds[1].trim(), "-gdb-set mi-async on");
    }

    #[test]
    fn frame_filter_pattern_conversion_preserves_match_type() {
        let pattern = FrameFilterPatternConfig {
            pattern: "runtime::*".to_string(),
            match_type: FrameFilterMatchType::Glob,
        };

        let add_args: FrameFilterAddArgs = (&pattern).into();
        assert_eq!(add_args.to_string(), "runtime::* --match-type glob");
    }
}
