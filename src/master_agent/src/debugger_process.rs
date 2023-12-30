use std::process::{Child, Command, Stdio, Output, ChildStdin, ChildStdout};
use crate::launch_option::LaunchOption;

pub struct DebuggerProcess {
    option: LaunchOption,
    process: Option<Child>
}

impl DebuggerProcess {
    pub fn new(option: LaunchOption) -> Self {
        DebuggerProcess {
            option,
            process: None
        }
    }

    pub fn start(&mut self) {
        let child = Command::new(self.option.get_debugger_path())
            // .arg("../bin/hello_world")
            // .arg("--interpreter=mi")
            .args(self.option.get_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // .stdin(cfg)
            // .stdin(Stdio::piped())
            .spawn()
            // .output()
            .expect("Failed to start process");

        self.process = Some(child);
    }

    pub fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.kill();
        }
    }
}

