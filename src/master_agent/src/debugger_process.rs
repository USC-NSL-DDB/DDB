// use std::error::Error;
use std::process::{Child, Command, Stdio, Output, ChildStdin, ChildStdout, ChildStderr};
use std::{str, fmt};
use std::io::{
    Write, Read, self 
};
use tracing::{info, warn};

use crate::launch_option::LaunchOption;

#[derive(Debug)]
pub enum Error {
    // Failed to perform IO operation on stdin/stdout/stderr
    IOError(io::Error),
    PipeError,
    // Failed to start or stop debugger process
    OperationError,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::IOError(e) => write!(f, "failed to perform IO operation on stdin/stdout/stderr: {}", e),
            Error::PipeError => write!(f, "failed to get usable pipe for stdin/stdout/stderr"),
            Error::OperationError => write!(f, "failed to start or stop debugger process"),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

// NOTE: current implementation for stdout is problematic.
// The notification is async. Therefore, we shouldn't block the stdin while waiting for the stdout.
#[derive(Default)]
pub struct DebuggerProcess {
    option: LaunchOption,
    process: Option<Child>,
    c_stdin: Option<ChildStdin>,
    c_stdout: Option<ChildStdout>,
    c_stderr: Option<ChildStderr>
}

impl DebuggerProcess {
    pub fn new(option: LaunchOption) -> Self {
        DebuggerProcess {
            option,
            ..Default::default()
        }
    }

    pub fn start(&mut self) -> Result<()> {
        let mut child = Command::new(self.option.get_debugger_path())
            // .arg("../bin/hello_world")
            // .arg("--interpreter=mi")
            .args(self.option.get_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // .stdin(cfg)
            // .stdin(Stdio::piped())
            .spawn();
            // .output()
        
        if let Ok(mut child) = child {
            self.c_stdin = child.stdin.take();
            self.c_stdout = child.stdout.take();
            self.c_stderr = child.stderr.take();
            self.process = Some(child);
            return Ok(());
        }
        Err(Error::OperationError)
    }

    pub fn kill(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.kill();
        }

        self.c_stdin.take();
        self.c_stdout.take();
        self.c_stderr.take();
    }

    pub fn read(&mut self) -> Result<String> {
        if let Some(c_stdout) = &mut self.c_stdout {
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

                        // Print characters as ascii values
                        // for character in out_str.chars() {
                        //     print!("{} ", character as u8);
                        // }

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
            return Ok(out_str);
        }
        Err(Error::PipeError) 
    }

    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        // match &mut self.c_stdin {
        //     Some(c_stdin) => {
        //         c_stdin.write_all(buf).expect("Failed to write to child stdin");
        //         c_stdin.flush().expect("Failed to flush child stdin");
        //         Ok(())
        //     },
        //     _ => {

        //     }
        // }
        if let Some(c_stdin) = &mut self.c_stdin {
            c_stdin.write_all(buf).expect("Failed to write to child stdin");
            c_stdin.flush().expect("Failed to flush child stdin");
            return Ok(());
        }
        Err(Error::PipeError)
    }
}

