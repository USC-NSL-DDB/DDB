use std::borrow::BorrowMut;
use std::os::linux::raw;
// use std::error::Error;
use std::process::{Child, Command, Stdio, Output, ChildStdin, ChildStdout, ChildStderr};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::{str, fmt, thread};
use std::io::{
    Write, Read, self, BufWriter, BufReader, BufRead 
};
use tracing::{info, warn};

use crate::launch_option::LaunchOption;

#[derive(Debug, thiserror::Error)]
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

pub trait Communicatable {
    type Target;
    fn send(&self, content: &String);
    fn recv(&self) -> Self::Target;
}

pub struct Communicator {
    c_stdin: Option<BufWriter<ChildStdin>>,
    c_stdout: Option<BufReader<ChildStdout>>,
    c_stderr: Option<BufReader<ChildStderr>>,
    stdout_handle: Option<JoinHandle<()>>,
    stderr_handle: Option<JoinHandle<()>>,
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<String>,
}

impl Default for Communicator {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { 
            c_stdin: Default::default(), 
            c_stdout: Default::default(), 
            c_stderr: Default::default(), 
            stdout_handle: None,
            stderr_handle: None,
            tx,
            rx,
        }
    }
}

impl Communicatable for Communicator {
    type Target = String;

    fn send(&self, content: &String) {
        self.tx.send(content.clone());
        // if let Some(c_stdin) = self.c_stdin.as_mut() {
        //     c_std
        // }
    }

    fn recv(&self) -> Self::Target {
        self.rx.recv().unwrap()
    }
}

impl Communicator {
    pub fn set_pipeline(
        &mut self, 
        mut stdin: Option<ChildStdin>,
        mut stdout: Option<ChildStdout>,
        mut stderr: Option<ChildStderr>
    ) {
        self.c_stdin = stdin.take().map(|stdin| BufWriter::new(stdin));
        self.c_stdout = stdout.take().map(|stdout| BufReader::new(stdout));
        self.c_stderr = stderr.take().map(|stderr| BufReader::new(stderr));
    }

    pub fn cleanup(&mut self) {
        self.c_stdin.take();
        self.c_stdout.take();
        self.c_stderr.take();
        self.stdout_handle.take();
        self.stderr_handle.take();
    }

    pub fn start_listen(&mut self) {
        if let Some(c_stdout) = self.c_stdout.borrow_mut() {
            let stdout_tx = self.tx.clone();
            self.stdout_handle = Some(
                thread::spawn(|| {
                    let mut line = String::new();
                    while let Ok(size) = c_stdout.read_line(&mut line) {
                        if size != 0 {
                            println!("size != 0");
                            stdout_tx.send(line);
                        } else {
                            println!("size == 0");
                        }
                        line.clear();
                    }
                })
            );
        }

        if let Some(c_stderr) = self.c_stderr.borrow_mut() {
            self.stderr_handle = Some(
                thread::spawn(|| {
                    let mut line = String::new();
                    while let Ok(size) = c_stderr.read_line(&mut line) {
                        if size != 0 {
                            println!("size != 0");
                            eprintln!("stderr: {}", line);
                        } else {
                            println!("size == 0");
                        }
                        line.clear();
                    }
                })
            );
        }
    }
}

// NOTE: current implementation for stdout is problematic.
// The notification is async. Therefore, we shouldn't block the stdin while waiting for the stdout.
#[derive(Default)]
pub struct DebuggerProcess {
    option: LaunchOption,
    process: Option<Child>,
    comm: Communicator
    // c_stdin: Option<BufWriter<ChildStdin>>,
    // c_stdout: Option<BufReader<ChildStdout>>,
    // c_stderr: Option<BufReader<ChildStderr>>,
}

impl DebuggerProcess {
    pub fn new(option: LaunchOption) -> Self {
        DebuggerProcess {
            option,
            process: None,
            comm: Default::default()
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
            .spawn()
            .map_err(|_| Error::OperationError)?;
            // .output()

        self.comm.set_pipeline(child.stdin, child.stdout, child.stderr);
        self.process = Some(child);
        Ok(())
    }

    pub fn kill(&mut self) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.kill().map_err(|_| Error::OperationError)?;
        }

        self.comm.cleanup();
        Ok(())
    }

    pub fn read_until(&mut self, sender: mpsc::Sender<String>) -> Result<()> {
        let mut line = String::new();
        let c_stdout = self.c_stdout.as_mut().ok_or(Error::PipeError)?;
        while let Ok(size) = c_stdout.read_line(&mut line) {
            if size != 0 {
                println!("size != 0");
                sender.send(line.clone()).unwrap();
            } else {
                println!("size == 0");
            }
            line.clear();
        }
        // c_stdout.read_line(&mut line).map_err(|e| Error::IOError(e))?;
        Ok(())
    }

    pub fn read(&mut self) -> Result<String> {
        let mut line = String::new();
        // self.c_stdout.read
        let c_stdout = self.c_stdout.as_mut().ok_or(Error::PipeError)?;
        c_stdout.read_line(&mut line).map_err(|e| Error::IOError(e))?;
        Ok(line)
        // let mut read_buf = [0u8; 512];
        // let mut out_str = String::new();
        // loop {
        //     info!("start new loop");
        //     match c_stdout.read(&mut read_buf) {
        //         Ok(size) => {
        //             if size == 0 { 
        //                 info!("break: size == 0");
        //                 break;
        //             }
        //             let partial_read = str::from_utf8(&read_buf[..size]).expect("Failed to parse the read buf");
        //             out_str += partial_read;

        //             // Print characters as ascii values
        //             // for character in out_str.chars() {
        //             //     print!("{} ", character as u8);
        //             // }

        //             if Self::is_full_output(&out_str) 
        //             { 
        //                 info!("break: pass end check");
        //                 break;
        //             }
        //             read_buf.fill(0);
        //         },
        //         Err(err) => warn!("Failed to read from child stdout. {}", err),
        //     }
        // }
        // Ok(out_str)
    }

    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        let c_stdin = self.c_stdin.as_mut().ok_or(Error::PipeError)?;
        c_stdin.write_all(buf).expect("Failed to write to child stdin");
        c_stdin.flush().expect("Failed to flush child stdin");
        Ok(())
    }

    pub fn write_raw_cmd(&mut self, raw_cmd: &str) -> Result<()> {
        let c_stdin = self.c_stdin.as_mut().ok_or(Error::PipeError)?;
        if raw_cmd.ends_with("\n") {
            write!(c_stdin, "{}", raw_cmd).map_err(|e| Error::IOError(e))?;
        } else {
            write!(c_stdin, "{}\n", raw_cmd).map_err(|e| Error::IOError(e))?;
        }
        Ok(())
    }

    #[inline(always)]
    fn is_full_output(output: &str) -> bool {
        output.ends_with("(gdb) \r") 
            || output.ends_with("(gdb) \r\n") 
            || output.ends_with("(gdb) \n") 
    }
}

impl Drop for DebuggerProcess {
    fn drop(&mut self) {
        if let Some(stdin) = self.c_stdin.take().as_mut() {
           stdin.write_all(b"-gdb-exit\n");
        }

        self.c_stdout.take();
        self.c_stderr.take();
        if let Some(process) = self.process.take().as_mut() {
            process.kill();
        }
    }
}
