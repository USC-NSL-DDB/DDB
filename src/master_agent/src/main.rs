// Master Agent itself should also support GDB/MI
// so that vs code adapter can support our master agent.

use std::process::{Child, Command, Stdio, Output};
use std::io;

struct LaunchOptionArgs {
    args: &'static [&'static str]
}

impl LaunchOptionArgs {
    fn new(args: &'static [&'static str]) -> LaunchOptionArgs {
        LaunchOptionArgs { args }
    }

    fn get_args(&self) -> Vec<&str> {
        let mut arg_vec = self.args.to_vec();
        arg_vec.push("--interpreter=mi");
        arg_vec
    }
}

struct LaunchOption {
    mi_debugger_path: &'static str,
    args: LaunchOptionArgs
}

impl LaunchOption {
    fn new(
        mi_debugger_path: &'static str, 
        args: &'static [&'static str]
    ) -> LaunchOption {
        LaunchOption {
            mi_debugger_path,
            args: LaunchOptionArgs::new(args)
        }
    }

    fn get_args(&self) -> Vec<&str> {
        self.args.get_args()
    }
}

fn start_process(option: LaunchOption) -> Child {
    let output = Command::new(option.mi_debugger_path)
        // .arg("../bin/hello_world")
        // .arg("--interpreter=mi")
        .args(option.get_args())
        // .stdin(cfg)
        // .stdin(Stdio::piped())
        .spawn()
        // .output()
        .expect("Failed to start process");

    // child
    output
}

fn main() {
    // for now, just directly use MI mode

    // let stdin = io::stdin().lock();

    let option = LaunchOption::new(
        "gdb",
        &[
            "../bin/hello_world"
        ]
    );

    let mut output = start_process(option);
    output.wait();
    println!("Output: {output:?}");
}
