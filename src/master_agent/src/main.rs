// Master Agent itself should also support GDB/MI
// so that vs code adapter can support our master agent.

use std::process::{Child, Command, Stdio, Output};
use std::io;

fn start_process() -> Child {
    let output = Command::new("gdb")
        .arg("../bin/hello_world")
        .arg("--interpreter=mi")
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

    let mut output = start_process();
    output.wait();
    println!("Output: {output:?}");
}
