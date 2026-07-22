mod api;
mod app;
mod arg;
mod cmd_flow;
mod common;
mod connection;
mod dbg_ctrl;
mod dbg_mgr;
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
mod source;
mod state;
mod status;

use std::sync::Arc;

use anyhow::Result;
use app::ApplicationRuntime;
use clap::Parser;
use common::config::Config;
use debugger::resolve_debugger_backend;
use plugin::resolve_framework_plugin;
use rust_embed::Embed;
use setup::{AppDirConfig, LoggingSettings, SetupProcedure};
use tracing::{debug, info};

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

#[tokio::main]
async fn main() -> Result<()> {
    let args = arg::Args::parse();
    let logging_settings = LoggingSettings::from_args(&args);
    let command_workers = args.command_workers;
    let config = Arc::new(Config::load(args.config)?);
    let backend = resolve_debugger_backend(config.as_ref())?;
    let plugin = resolve_framework_plugin(config.as_ref());
    let app_dir_conf = AppDirConfig::from_config(config.as_ref());

    // Directories, logging, and bundled assets must exist before the service
    // graph is built: construction performs network I/O and emits tracing.
    let tracing_guards = SetupProcedure::new(
        Arc::clone(&config),
        Arc::clone(&backend),
        Arc::clone(&plugin),
    )
    .with_app_dir_config(app_dir_conf)
    .with_logging_settings(logging_settings)
    .run()?;

    let runtime = ApplicationRuntime::new(config, plugin, backend).await?;
    runtime.run(command_workers).await?;

    tracing_guards.shutdown();

    debug!("Exiting due to {:?}", runtime.shutdown().cause());
    info!("Bye!");
    Ok(())
}
