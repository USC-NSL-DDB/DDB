use crate::common::config::GdbCommand;

use super::DbgCmdGenerator;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum GdbCmd {
    // "-gdb-set <option> <value>"
    SetOption(GdbOption),
    // "-interpreter-exec console <cmd>"
    ConsoleExec(String),
    // "-target-attach <id>"
    TargetAttach(u64),
    // "-file-exec-and-symbols <bin_path>"
    FileExecAndSym(String),
    // "-exec-arguments <args_list>"
    ExeArgs(String),
    // Plain commands (usually from user inputs or hardcoded commands)
    Plain(String),
}

impl From<GdbCommand> for GdbCmd {
    fn from(cmd: GdbCommand) -> Self {
        GdbCmd::Plain(cmd.command)
    }
}

impl From<&GdbCommand> for GdbCmd {
    fn from(cmd: &GdbCommand) -> Self {
        GdbCmd::Plain(cmd.command.clone())
    }
}

impl Into<GdbCommand> for GdbCmd {
    fn into(self) -> GdbCommand {
        let cmd = self.generate();
        GdbCommand {
            name: "unnamed cmd".to_string(),
            command: cmd,
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
        };

        // GDB command needs to be ended with '\n'
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
    Logging(bool),
    MiAsync(bool),
}

impl DbgCmdGenerator for GdbOption {
    fn generate(&self) -> String {
        match self {
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
    use super::super::DbgCmdListBuilder;
    use super::*;

    #[test]
    fn test_gdb_cmd_generate() {
        let cmd = GdbCmd::SetOption(GdbOption::Logging(true));
        assert_eq!(cmd.generate(), "-gdb-set logging enabled on\n");

        let cmd = GdbCmd::ConsoleExec("info registers".to_string());
        assert_eq!(
            cmd.generate().trim(),
            r#"-interpreter-exec console "info registers""#
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
        assert_eq!(cmds.len(), 7);
        assert_eq!(cmds[0].trim(), "-gdb-set logging enabled on");
        assert_eq!(cmds[1].trim(), "-gdb-set mi-async on");
        assert_eq!(cmds[2].trim(), r#"-interpreter-exec console "info registers""#);
        assert_eq!(cmds[3].trim(), "-target-attach 1234");
        assert_eq!(cmds[4].trim(), "-file-exec-and-symbols /path/to/bin");
        assert_eq!(cmds[5].trim(), "-exec-arguments arg1 arg2");
        assert_eq!(cmds[6].trim(), "target remote localhost:1234");
    }
}
