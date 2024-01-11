// Master Agent itself should also support GDB/MI
// so that vs code adapter can support our master agent.

///
/// NOTE:
/// 1. https://github.com/kkawakam/rustyline 
///    Rustyline might be useful to use to build the cli.
/// 2. 

mod dbg;
mod launch_option;
mod output;
mod parser;
// mod transports;

use tracing_subscriber;
use std::os::unix::thread;
use std::process::{Child, Command, Stdio, Output, ChildStdin, ChildStdout};
use std::io::{self, Write, Read};

use crate::launch_option::LaunchOption;
use crate::dbg::DebuggerProcess;

fn main() {
    tracing_subscriber::fmt::init();
    // for now, just directly use MI mode

    let option = LaunchOption::new(
        "gdb",
        &[
            "../bin/hello_world"
        ]
    );

    let mut debugger_p = DebuggerProcess::new(option);
    debugger_p.start().unwrap();

    let mut input = String::new();
    println!("Type something and press enter. Type 'exit' to quit.");

    let response = debugger_p.read().unwrap();
    print!("{}", response);

    // info!("Finished reading from stdout.");
    use std::sync::mpsc::{self, Sender, Receiver};

    let (tx, rx) = mpsc::channel::<String>();

    std::thread::spawn(move || {
        println!("In new thread");
        debugger_p.read_until(tx.clone());
    });

    std::thread::spawn(move || {
        while let Ok(output) = rx.recv()  {
            println!("Begin output");
            println!("{}", output);
        }
    });

    println!("Out wait loop");

    // while io::stdin().read_line(&mut input).expect("Failed to read line") > 0 {
    //     if input.trim() == "exit" || input.trim() == "quit" {
    //         debugger_p.kill().unwrap();
    //         break;
    //     }

    //     debugger_p.write_all(input.as_bytes()).unwrap();

    //     input.clear(); // Clear the buffer for the next input

    //     let response = debugger_p.read().unwrap();
    //     print!("{}", response);
    //     // debugger_p.read_until(sender)
    // }

    // output.wait();
    // println!("Output: {output:?}");
}
