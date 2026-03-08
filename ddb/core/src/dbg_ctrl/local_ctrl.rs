use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

use crate::connection::SSHIo;

use super::{DbgControllable, InputReceiver, OutputSender};

#[derive(Debug)]
pub struct LocalProcessController {
    child: Option<Child>,
    input_handle: Option<tokio::task::JoinHandle<()>>,
    output_handles: Vec<tokio::task::JoinHandle<()>>,
    open: Arc<AtomicBool>,
}

impl LocalProcessController {
    pub fn new() -> Self {
        Self {
            child: None,
            input_handle: None,
            output_handles: Vec::new(),
            open: Arc::new(AtomicBool::new(false)),
        }
    }

    fn parse_command(cmd: &str) -> Result<(String, Vec<String>)> {
        let parts = cmd
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let program = parts
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("local controller command is empty"))?;
        Ok((program, parts[1..].to_vec()))
    }

    async fn forward_input(
        mut stdin: tokio::process::ChildStdin,
        in_rx: InputReceiver,
        open: Arc<AtomicBool>,
    ) {
        while let Ok(data) = in_rx.recv_async().await {
            if stdin.write_all(data.as_ref()).await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
        let _ = stdin.shutdown().await;
        open.store(false, Ordering::SeqCst);
    }

    async fn forward_output<R>(
        reader: R,
        out_tx: OutputSender,
        open: Arc<AtomicBool>,
        remaining_readers: Arc<AtomicUsize>,
    ) where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if out_tx.send_async(Bytes::from(line)).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if remaining_readers.fetch_sub(1, Ordering::SeqCst) == 1 {
            open.store(false, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl DbgControllable for LocalProcessController {
    type InputType = Bytes;

    async fn start(&mut self, cmd: &str) -> Result<SSHIo> {
        let (program, args) = Self::parse_command(cmd)?;
        let mut child = Command::new(program);
        child
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = child.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to capture local debugger stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to capture local debugger stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to capture local debugger stderr"))?;

        let (in_tx, in_rx) = flume::bounded::<Bytes>(1024);
        let (out_tx, out_rx) = flume::bounded::<Bytes>(1024);
        let remaining_readers = Arc::new(AtomicUsize::new(2));

        self.open.store(true, Ordering::SeqCst);
        self.input_handle = Some(tokio::spawn(Self::forward_input(
            stdin,
            in_rx,
            Arc::clone(&self.open),
        )));
        self.output_handles.push(tokio::spawn(Self::forward_output(
            stdout,
            out_tx.clone(),
            Arc::clone(&self.open),
            Arc::clone(&remaining_readers),
        )));
        self.output_handles.push(tokio::spawn(Self::forward_output(
            stderr,
            out_tx,
            Arc::clone(&self.open),
            remaining_readers,
        )));
        self.child = Some(child);
        Ok(SSHIo { in_tx, out_rx })
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    async fn close(&mut self) -> Result<()> {
        self.open.store(false, Ordering::SeqCst);

        if let Some(handle) = self.input_handle.take() {
            handle.abort();
        }
        for handle in self.output_handles.drain(..) {
            handle.abort();
        }

        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                child.kill().await.ok();
            }
            child.wait().await.ok();
        }
        self.child = None;
        Ok(())
    }
}
