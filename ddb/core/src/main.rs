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
mod launcher_dispatch;
mod logging;
mod notification;
mod plugin;
mod session;
mod setup;
mod shutdown;
mod source;
mod startup;
mod state;
mod status;

use std::sync::Arc;

use anyhow::{Context, Result};
use app::{ApplicationRuntime, RuntimeConstructionOptions, RuntimeRunOptions};
use arg::{Args, Command};
use clap::Parser;
use common::config::Config;
use debugger::resolve_debugger_backend;
use plugin::resolve_framework_plugin;
use rust_embed::Embed;
use setup::{AppDirConfig, LoggingSettings, SetupProcedure};
use startup::{BackendStartup, StartupReporter};
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
    let args = Args::parse();
    let logging = LoggingSettings::from_args(&args.logging);
    let command_workers = args.command_workers;

    match args.command {
        Some(Command::Tui(tui)) => {
            let status = launcher_dispatch::dispatch(&tui)?;
            if status != 0 {
                std::process::exit(status);
            }
            Ok(())
        }
        Some(Command::Serve(serve)) => {
            if args.config.is_some() {
                anyhow::bail!(
                    "a root configuration path cannot be combined with the serve subcommand"
                );
            }
            let early_reporter = serve
                .startup_report
                .as_ref()
                .map(|path| StartupReporter::new(path.clone()));
            if let Some(reporter) = &early_reporter {
                reporter.set_phase("config_loading");
            }
            let startup = match BackendStartup::serve(&serve, early_reporter.clone()) {
                Ok(startup) => startup,
                Err(error) => {
                    if let Some(reporter) = &early_reporter {
                        if let Err(report_error) = reporter.failed(&error) {
                            eprintln!(
                                "failed to publish DDB startup error to {}: {report_error:#}",
                                serve
                                    .startup_report
                                    .as_deref()
                                    .expect("reporter has a path")
                                    .display()
                            );
                        }
                    }
                    return Err(error);
                }
            };
            run_backend(startup, logging, command_workers).await
        }
        None => {
            let config = Config::load(args.config)?;
            run_backend(BackendStartup::legacy(config), logging, command_workers).await
        }
    }
}

async fn run_backend(
    startup: BackendStartup,
    logging: LoggingSettings,
    command_workers: usize,
) -> Result<()> {
    let reporter = startup.reporter.clone();
    let result = run_backend_inner(startup, logging, command_workers).await;
    if let (Some(reporter), Err(error)) = (&reporter, &result) {
        if let Err(report_error) = reporter.failed(error) {
            eprintln!("failed to publish DDB startup failure: {report_error:#}");
        }
    }
    result
}

async fn run_backend_inner(
    startup: BackendStartup,
    logging: LoggingSettings,
    command_workers: usize,
) -> Result<()> {
    let config = Arc::new(startup.config);

    if let Some(reporter) = &startup.reporter {
        reporter.set_phase("debugger_resolution");
    }
    let backend = resolve_debugger_backend(config.as_ref())?;
    if startup.preflight_debugger {
        debugger::preflight_debugger_backend(backend.as_ref()).await?;
    }
    if let Some(reporter) = &startup.reporter {
        reporter.set_phase("config_validation");
    }
    for (index, session) in config.static_sessions.iter().enumerate() {
        session::factory::validate_static_session_config(config.as_ref(), session)
            .with_context(|| format!("invalid StaticSessions[{index}]"))?;
    }
    api::security::validate_api_deployment_with_options(
        config.as_ref(),
        startup.allow_ephemeral_api_port,
    )?;
    let plugin = resolve_framework_plugin(config.as_ref());
    let app_dir_conf = AppDirConfig::from_config(config.as_ref());

    if let Some(reporter) = &startup.reporter {
        reporter.set_phase("filesystem_setup");
    }
    // Directories, logging, and bundled assets must exist before the service
    // graph is built: construction performs network I/O and emits tracing.
    let tracing_guards = SetupProcedure::new(Arc::clone(&config), Arc::clone(&backend))
        .with_app_dir_config(app_dir_conf)
        .with_logging_settings(logging)
        .run()?;

    if let Some(reporter) = &startup.reporter {
        reporter.set_phase("service_startup");
    }
    let runtime = ApplicationRuntime::new_with_options(
        config,
        plugin,
        backend,
        RuntimeConstructionOptions {
            allow_ephemeral_api_port: startup.allow_ephemeral_api_port,
        },
    )
    .await?;

    if let Some(reporter) = &startup.reporter {
        reporter.set_phase("service_startup");
    }
    let run_result = runtime
        .run_with_options(RuntimeRunOptions {
            interactive: startup.interactive,
            command_workers,
            remove_auth_token_after_load: startup.remove_auth_token_after_load,
            startup_reporter: startup.reporter,
        })
        .await;

    tracing_guards.shutdown();

    run_result?;
    debug!("Exiting due to {:?}", runtime.shutdown().cause());
    info!("Bye!");
    Ok(())
}
