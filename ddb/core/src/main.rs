mod api;
mod app;
mod arg;
mod cmd_flow;
mod common;
mod connection;
mod context;
mod dbg_ctrl;
mod dbg_mgr;
mod debugger;
mod discovery;
mod feature;
mod global;
mod group_operation;
mod logging;
mod notification;
mod plugin;
mod runtime_model;
mod session;
mod setup;
mod shutdown;
mod source;
mod state;
mod status;

use std::sync::Arc;

use anyhow::Result;
use app::App;
use clap::Parser;
use cmd_flow::{engine::CommandEngine, format_error};
use common::config::Config;
use console_subscriber;
use context::ApplicationRuntime;
use dbg_mgr::DbgManager;
use debugger::resolve_debugger_backend;
use notification::NotificationManager;
use plugin::resolve_framework_plugin;
use rust_embed::Embed;
use setup::{AppDirConfig, LoggingSettings, SetupProcedure};
use shutdown::{ShutdownCause, ShutdownCtrl};
use status::{Component, RuntimeStatus};
use tokio::{
    io::{self, AsyncBufReadExt},
    task::JoinSet,
};
use tracing::{debug, error, info};

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

async fn run_cmd_loop(
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

async fn run_command_flow(status: Arc<RuntimeStatus>, shutdown: Arc<ShutdownCtrl>) -> Result<()> {
    status.up(Component::CmdFlow);
    shutdown.wait_for_exit().await;
    Ok(())
}

async fn run_debugger_manager(
    manager: Arc<DbgManager>,
    status: Arc<RuntimeStatus>,
    shutdown: Arc<ShutdownCtrl>,
) -> Result<()> {
    if let Err(error) = manager.start().await {
        error!("[DbgManager]: Failed to start: {:?}", error);
        shutdown.trigger_once(ShutdownCause::DbgMgrInitFailure);
        shutdown.shutdown_cleanup(manager.cleanup()).await;
        return Err(error);
    }

    debug!("[DbgManager]: Started successfully.");
    status.up(Component::DbgMgr);
    shutdown.wait_for_exit().await;
    shutdown.shutdown_cleanup(manager.cleanup()).await;
    Ok(())
}

async fn run_notification_manager(
    manager: Arc<NotificationManager>,
    status: Arc<RuntimeStatus>,
    shutdown: Arc<ShutdownCtrl>,
) -> Result<()> {
    manager.start().await;
    status.up(Component::Notification);
    shutdown.wait_for_exit().await;
    shutdown.shutdown_cleanup(manager.shutdown()).await;
    Ok(())
}

async fn run_main(runtime: &ApplicationRuntime, command_workers: usize) -> Result<()> {
    let services = Arc::clone(runtime.services());
    let shutdown = Arc::clone(runtime.shutdown());
    let status = Arc::clone(runtime.status());
    let app = App::new(
        services.config().conf.api_server_port,
        services.as_ref(),
        Arc::clone(&shutdown),
        Arc::clone(&status),
    );
    let mut tasks = JoinSet::new();

    {
        let status = Arc::clone(&status);
        let shutdown = Arc::clone(&shutdown);
        tasks.spawn(async move { ("command-flow", run_command_flow(status, shutdown).await) });
    }
    {
        let manager = Arc::clone(services.debugger_manager());
        let status = Arc::clone(&status);
        let shutdown = Arc::clone(&shutdown);
        tasks.spawn(async move {
            (
                "debugger-manager",
                run_debugger_manager(manager, status, shutdown).await,
            )
        });
    }
    {
        let manager = Arc::clone(services.notification_manager());
        let status = Arc::clone(&status);
        let shutdown = Arc::clone(&shutdown);
        tasks.spawn(async move {
            (
                "notification-manager",
                run_notification_manager(manager, status, shutdown).await,
            )
        });
    }
    {
        let stop = shutdown.subscribe();
        tasks.spawn(async move { ("api-server", app.run(stop).await) });
    }
    {
        let shutdown = Arc::clone(&shutdown);
        tasks.spawn(async move {
            shutdown.wait_for_signal().await;
            ("signal-handler", Ok(()))
        });
    }
    {
        let engine = Arc::clone(services.command_engine());
        let status = Arc::clone(&status);
        let shutdown_for_loop = Arc::clone(&shutdown);
        let stop = shutdown.subscribe();
        tasks.spawn(async move {
            ("command-loop", {
                run_cmd_loop(engine, command_workers, status, shutdown_for_loop, stop).await;
                Ok(())
            })
        });
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((name, result)) => {
                if let Err(error) = result {
                    error!("[Runtime]: component {} failed: {:?}", name, error);
                } else {
                    debug!("[Runtime]: component {} stopped", name);
                }
                if !shutdown.should_shutdown() {
                    error!("[Runtime]: component {} stopped unexpectedly", name);
                    shutdown.trigger_once(ShutdownCause::Other);
                }
            }
            Err(error) => {
                error!("[Runtime]: component task failed: {:?}", error);
                shutdown.trigger_once(ShutdownCause::Other);
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = arg::Args::parse();
    let command_workers = args.command_workers;
    let logging_settings = LoggingSettings::from_args(&args);
    let config = Arc::new(Config::load(args.config)?);
    let backend = resolve_debugger_backend(config.as_ref());
    let plugin = resolve_framework_plugin(config.as_ref());
    let runtime = ApplicationRuntime::new(
        Arc::clone(&config),
        Arc::clone(&plugin),
        Arc::clone(&backend),
    )
    .await?;
    let app_dir_conf = AppDirConfig::from_config(config.as_ref());

    let tracing_guards = SetupProcedure::new(config, backend, plugin)
        .with_app_dir_config(app_dir_conf)
        .with_logging_settings(logging_settings)
        .run()?;

    run_main(&runtime, command_workers).await?;

    tracing_guards.shutdown();

    debug!("Exiting due to {:?}", runtime.shutdown().cause());
    info!("Bye!");
    Ok(())
}
