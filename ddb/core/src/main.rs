mod api;
mod app;
mod arg;
mod cmd_flow;
mod common;
mod connection;
mod context;
mod dbg_cmd;
mod dbg_ctrl;
mod dbg_mgr;
mod dbg_parser;
mod debugger;
mod discovery;
mod feature;
mod global;
mod logging;
mod notification;
mod plugin;
mod session;
mod setup;
mod shutdown;
mod state;
mod status;

use std::sync::Arc;
use std::sync::Weak;

use app::App;
use cmd_flow::get_cmd_handler;
use common::config::Config;
use dbg_mgr::DbgManagable;
use dbg_mgr::DbgManager;
use debugger::{init_debugger_backend, resolve_debugger_backend};
use plugin::{get_framework_plugin, init_framework_plugin, resolve_framework_plugin};
use setup::LoggingSettings;
use setup::{AppDirConfig, SetupProcedure};
use shutdown::{get_shutdown_ctrl, ShutdownCause, ShutdownCtrl};
use status::*;

use anyhow::Result;
use clap::Parser;
use console_subscriber;
use rust_embed::Embed;
use tokio::io::{self, AsyncBufReadExt};
use tokio::task::JoinSet;
use tracing::error;
use tracing::{debug, info};

#[cfg(feature = "profile")]
use tracing::instrument;

#[derive(Embed)]
#[folder = "assets/"]
struct Asset;

#[cfg(debug_assertions)]
#[allow(dead_code)]
fn init_console_subscriber() {
    console_subscriber::init();
}

#[cfg(not(debug_assertions))]
#[allow(dead_code)]
fn init_console_subscriber() {
    // No-op in release builds
}

async fn run_cmd_loop(mut stop_sig: tokio::sync::watch::Receiver<bool>) {
    // wait for all components to be up to receive input
    // Or immediately exit if stop signal is received
    tokio::select! {
        _ = stop_sig.changed() => {
            debug!("Exiting command loop before starting, stop signal received.");
            return;
        }
        _ = status::get_rt_status().wait_for_up() => {}
    }

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin).lines();

    loop {
        println!("(ddb) ");
        tokio::select! {
            _ = stop_sig.changed() => {
                println!("Received stop signal, exiting command loop...");
                break;
            }
            // Read a line from stdin
            line = reader.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let input = line.trim();
                        if input.is_empty() {
                            // ignore empty inputs
                            continue;
                        }
                        if input == "exit" {
                            get_shutdown_ctrl().trigger_once(ShutdownCause::UserExit);
                            println!("Exiting command loop...");
                            break;
                        }
                        get_cmd_handler().input(input).await;
                    }
                    Ok(None) => {
                        get_shutdown_ctrl().trigger_once(ShutdownCause::StdinEof);
                        println!("EOF reached, exiting command loop...");
                        break;
                    }
                    Err(err) => {
                        get_shutdown_ctrl().trigger_once(ShutdownCause::StdinError);
                        eprintln!("Error reading line: {}", err);
                        break;
                    }
                }
            }
        }
    }
}

pub fn init_dbg_mgr<F>(f: F)
where
    F: FnOnce() -> Weak<DbgManager>,
{
    context::app_context().set_debugger_manager(f());
}

pub fn get_dbg_mgr() -> Arc<DbgManager> {
    context::app_context().debugger_manager()
}

async fn run_command_flow(command_workers: usize) -> Result<()> {
    let cmd_handler = Arc::clone(get_cmd_handler());
    cmd_handler.clone().start(command_workers);
    get_rt_status().up(Component::CmdFlow);

    ShutdownCtrl::wait_for_exit().await;
    get_shutdown_ctrl()
        .shutdown_cleanup(async {
            cmd_handler.stop();
        })
        .await;
    Ok(())
}

async fn run_debugger_manager() -> Result<()> {
    let dbg_mgr = Arc::new(DbgManager::new().await);
    init_dbg_mgr(|| Arc::downgrade(&dbg_mgr));

    if let Err(error) = dbg_mgr.start().await {
        error!("[DbgManager]: Failed to start: {:?}", error);
        get_shutdown_ctrl().trigger_once(ShutdownCause::DbgMgrInitFailure);
        get_shutdown_ctrl()
            .shutdown_cleanup(dbg_mgr.cleanup())
            .await;
        return Err(error);
    }

    debug!("[DbgManager]: Started successfully.");
    get_rt_status().up(Component::DbgMgr);
    ShutdownCtrl::wait_for_exit().await;
    get_shutdown_ctrl()
        .shutdown_cleanup(dbg_mgr.cleanup())
        .await;
    Ok(())
}

async fn run_notification_manager() -> Result<()> {
    let manager = notification::get_notif_mgr();
    manager.start().await;
    get_rt_status().up(Component::Notification);

    ShutdownCtrl::wait_for_exit().await;
    get_shutdown_ctrl()
        .shutdown_cleanup(manager.shutdown())
        .await;
    Ok(())
}

#[cfg_attr(feature = "profile", tracing::instrument(skip_all))]
async fn run_main(command_workers: usize) -> Result<()> {
    let app = App::new(Config::global().conf.api_server_port);
    let mut tasks = JoinSet::new();

    tasks.spawn(async move { ("command-flow", run_command_flow(command_workers).await) });
    tasks.spawn(async { ("debugger-manager", run_debugger_manager().await) });
    tasks.spawn(async { ("notification-manager", run_notification_manager().await) });
    tasks.spawn(async move { ("api-server", app.run(get_shutdown_ctrl().subscribe()).await) });
    tasks.spawn(async {
        get_shutdown_ctrl().wait_for_signal().await;
        ("signal-handler", Ok(()))
    });
    tasks.spawn(async {
        ("command-loop", {
            run_cmd_loop(get_shutdown_ctrl().subscribe()).await;
            Ok(())
        })
    });

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((name, result)) => {
                if let Err(error) = result {
                    error!("[Runtime]: component {} failed: {:?}", name, error);
                } else {
                    debug!("[Runtime]: component {} stopped", name);
                }
                if !get_shutdown_ctrl().should_shutdown() {
                    error!("[Runtime]: component {} stopped unexpectedly", name);
                    get_shutdown_ctrl().trigger_once(ShutdownCause::Other);
                }
            }
            Err(error) => {
                error!("[Runtime]: component task failed: {:?}", error);
                get_shutdown_ctrl().trigger_once(ShutdownCause::Other);
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // init_console_subscriber();
    let args = arg::Args::parse();
    let command_workers = args.command_workers;
    let logging_settings = LoggingSettings::from_args(&args);
    Config::init_global(args.config)?;
    init_debugger_backend(|| resolve_debugger_backend(Config::global()));
    init_framework_plugin(|| resolve_framework_plugin(Config::global()));
    context::init_app_context(get_framework_plugin().command_adapter());
    let app_dir_conf = AppDirConfig::from_config(Config::global());

    // Keep the guards to ensure the async logger and OTEL tracer are running.
    // The guards will be used for graceful shutdown at the end.
    let tracing_guards = SetupProcedure::new()
        .with_app_dir_config(app_dir_conf)
        .with_logging_settings(logging_settings)
        .run()?;

    run_main(command_workers).await?;

    // Gracefully shutdown the OpenTelemetry tracer to flush pending spans
    tracing_guards.shutdown();

    debug!("Exiting due to {:?}", get_shutdown_ctrl().cause());
    info!("Bye!");
    Ok(())
}
