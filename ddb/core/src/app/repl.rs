//! Interactive command loop reading user input from stdin.

use std::sync::Arc;

use tokio::{
    io::{self, AsyncBufReadExt},
    task::JoinSet,
};
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

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin).lines();
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
            line = reader.next_line(), if commands.len() < command_workers => {
                match line {
                    Ok(Some(line)) => {
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
                    Ok(None) => {
                        shutdown.trigger_once(ShutdownCause::StdinEof);
                        println!("EOF reached, exiting command loop...");
                        break;
                    }
                    Err(error) => {
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
