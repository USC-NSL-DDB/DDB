// Master Agent itself should also support GDB/MI
// so that vs code adapter can support our master agent.

use tracing::{info, warn};
use tracing_subscriber;
use std::process::{Child, Command, Stdio, Output, ChildStdin, ChildStdout};
use std::io::{self, Write, Read};
use std::str;

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
    let child = Command::new(option.mi_debugger_path)
        // .arg("../bin/hello_world")
        // .arg("--interpreter=mi")
        .args(option.get_args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // .stdin(cfg)
        // .stdin(Stdio::piped())
        .spawn()
        // .output()
        .expect("Failed to start process");

    child
}

fn read_mi_response(c_stdout: &mut ChildStdout) -> String {
    let mut read_buf = [0u8; 512];
    let mut out_str = String::new();
    loop {
        info!("start new loop");
        match c_stdout.read(&mut read_buf) {
            Ok(size) => {
                if size == 0 { 
                    info!("break: size == 0");
                    break;
                }
                let partial_read = str::from_utf8(&read_buf[..size]).expect("Failed to parse the read buf");
                out_str += partial_read;

                for character in out_str.chars() {
                    print!("{} ", character as u8);
                }
                if out_str.ends_with("(gdb) \r") 
                    || out_str.ends_with("(gdb) \r\n") 
                    || out_str.ends_with("(gdb) \n") 
                { 
                    info!("break: pass end check");
                    break;
                }
                read_buf.fill(0);
            },
            Err(err) => warn!("Failed to read from child stdout. {}", err),
        }
    }
    out_str
}

fn main() {
    tracing_subscriber::fmt::init();
    // for now, just directly use MI mode

    // let mut stdin_handle = io::stdin().lock();

    // let number_of_yaks = 3;
    // info!(number_of_yaks, "preparing to shave yaks");

    let option = LaunchOption::new(
        "gdb",
        &[
            "../bin/hello_world"
        ]
    );

    let mut child = start_process(option);
    
    let mut c_stdin = child.stdin.take().expect("Fail to setup stdin.");
    let mut c_stdout = child.stdout.take().expect("Fail to setup stdout.");

    let mut input = String::new();
    println!("Type something and press enter. Type 'exit' to quit.");

    let output = read_mi_response(&mut c_stdout);
    
    // c_stdout.read_to_string(&mut output).expect("failed to read from chil stdout");
    info!("Finished reading from stdout.");
    print!("{}", output);

    while io::stdin().read_line(&mut input).expect("Failed to read line") > 0 {
        if input.trim() == "exit" {
            break;
        }

        c_stdin.write_all(input.as_bytes()).expect("Failed to write to child stdin");
        c_stdin.flush().expect("Failed to flush child stdin");

        input.clear(); // Clear the buffer for the next input

        let output = read_mi_response(&mut c_stdout);
        print!("{}", output);
    }

    // output.wait();
    // println!("Output: {output:?}");
}
