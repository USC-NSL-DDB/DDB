//! Interactive command loop reading user input from stdin.

use std::sync::Arc;

use std::io::BufRead;

use tokio::{sync::mpsc, task::JoinSet};
use tracing::{debug, error};

use crate::{
    cmd_flow::{engine::CommandEngine, format_error},
    shutdown::{ShutdownCause, ShutdownCtrl},
    status::RuntimeStatus,
};

pub(super) async fn run(
    engine: Arc<CommandEngine>,
    command_workers: usize,
    status: Arc<RuntimeStatus>,
    shutdown: Arc<ShutdownCtrl>,
    mut stop_sig: tokio::sync::watch::Receiver<bool>,
) {
    tokio::select! {
        _ = stop_sig.changed() => {
            debug!("Exiting command loop before starting, stop signal received.");
            return;
        }
        _ = status.wait_for_up() => {}
    }

    let mut lines = stdin_lines();
    let mut commands = JoinSet::new();
    println!("(ddb) ");

    loop {
        tokio::select! {
            _ = stop_sig.changed() => {
                println!("Received stop signal, exiting command loop...");
                break;
            }
            joined = commands.join_next(), if !commands.is_empty() => {
                if let Some(Err(error)) = joined {
                    error!("[Command]: task failed: {:?}", error);
                }
            }
            line = lines.recv(), if commands.len() < command_workers => {
                match line {
                    Some(Ok(line)) => {
                        let input = line.trim();
                        if input.is_empty() {
                            println!("(ddb) ");
                            continue;
                        }
                        if input == "exit" {
                            shutdown.trigger_once(ShutdownCause::UserExit);
                            println!("Exiting command loop...");
                            break;
                        }
                        let engine = Arc::clone(&engine);
                        let command = input.to_string();
                        commands.spawn(async move {
                            match engine.execute_cli(&command).await {
                                Ok(outcome) => {
                                    for output in outcome.render_cli() {
                                        println!("{}", output);
                                        debug!("output: {}", output);
                                    }
                                }
                                Err(error) => {
                                    let output =
                                        format_error(&error.to_string(), error.external_token());
                                    println!("{}", output);
                                    debug!("output: {}", output);
                                }
                            }
                        });
                        println!("(ddb) ");
                    }
                    None => {
                        shutdown.trigger_once(ShutdownCause::StdinEof);
                        println!("EOF reached, exiting command loop...");
                        break;
                    }
                    Some(Err(error)) => {
                        shutdown.trigger_once(ShutdownCause::StdinError);
                        eprintln!("Error reading line: {}", error);
                        break;
                    }
                }
            }
        }
    }
    commands.abort_all();
    while commands.join_next().await.is_some() {}
}

/// Bridges blocking process stdin into the async command loop.
///
/// Tokio's stdin adapter delegates to the blocking pool, whose in-flight read
/// cannot be cancelled. A remote shutdown would therefore finish every DDB
/// component and then hang while the Tokio runtime waited for stdin EOF. A
/// detached OS thread is intentional here: dropping the receiver stops it
/// after the current read, and a blocked terminal read cannot delay process
/// termination.
fn stdin_lines() -> mpsc::Receiver<std::io::Result<String>> {
    let (sender, receiver) = mpsc::channel(1);
    std::thread::Builder::new()
        .name("ddb-stdin".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                if sender.blocking_send(line).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn stdin reader");
    receiver
}
