mod api;
mod app;
mod arg;
mod cmd_flow;
mod common;
mod connection;
mod dbg_cmd;
mod dbg_ctrl;
mod dbg_mgr;
mod dbg_parser;
mod discovery;
mod feature;
mod global;
mod logging;
mod session;
mod setup;
mod shutdown;
mod state;
mod status;

use std::sync::Arc;
use std::sync::OnceLock;

use app::App;
use cmd_flow::framework_adapter::*;
use cmd_flow::get_cmd_handler;
use cmd_flow::init_cmd_handler;
use cmd_flow::input::CmdHandler;
use common::config::Config;
use common::config::Framework;
use dbg_mgr::DbgManagable;
use dbg_mgr::DbgManager;
use setup::LoggingSettings;
use setup::{AppDirConfig, SetupProcedure};
use shutdown::{get_shutdown_ctrl, ShutdownCause, ShutdownCtrl};
use status::*;

use anyhow::Result;
use clap::Parser;
use console_subscriber;
use rust_embed::Embed;
use tokio::io::{self, AsyncBufReadExt};
use tracing::error;
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

static DBG_MGR: OnceLock<Arc<DbgManager>> = OnceLock::new();
pub fn init_dbg_mgr<F>(f: F)
where
    F: FnOnce() -> Arc<DbgManager>,
{
    DBG_MGR.get_or_init(f);
}

pub fn get_dbg_mgr() -> &'static Arc<DbgManager> {
    DBG_MGR.get().expect("DbgManager is not initialized.")
}

fn main() -> Result<()> {
    // init_console_subscriber();
    let args = arg::Args::parse();
    let logging_settings = LoggingSettings::from_args(&args);
    Config::init_global(args.config)?;
    let app_dir_conf = AppDirConfig::from_config(Config::global());

    get_shutdown_ctrl().setup_signal_handling();
    get_shutdown_ctrl().register_acks(&[Component::CmdFlow, Component::DbgMgr, Component::Api]);

    // Keep the guard to ensure the async logger is running.
    let _guard = SetupProcedure::new()
        .with_app_dir_config(app_dir_conf)
        .with_logging_settings(logging_settings)
        .run()?;

    let mut app = App::new(Config::global().conf.api_server_port);
    app.run(get_shutdown_ctrl().subscribe());

    init_cmd_handler(|| {
        let adapter: Arc<dyn FrameworkCommandAdapter> = match Config::global().framework {
            Framework::Nu => Arc::new(NuAdapter),
            Framework::GRPC => Arc::new(GrpcAdapter),
            Framework::ServiceWeaverKube => Arc::new(ServiceWeaverAdapter),
            _ => {
                panic!("Unsupported framework adapter for now.");
            }
        };
        CmdHandler::new(adapter)
    });

    let cmd_flow_handle = std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(5)
            .thread_name("dbg-cmd-flow")
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let tracker = cmd_flow::get_cmd_tracker();
            tracker.clone().start(10);

            let cmd_handler = get_cmd_handler();
            cmd_handler.clone().start(10);

            get_rt_status().up(Component::CmdFlow);

            ShutdownCtrl::wait_for_exit().await;
            get_shutdown_ctrl()
                .shutdown_cleanup(async {
                    cmd_handler.stop();
                    tracker.stop();
                })
                .await;
            get_shutdown_ctrl().ack_shutdown(Component::CmdFlow);
        });
    });

    // Start DbgManager in a separate thread with custom runtime
    let dbg_handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(5)
            .thread_name("dbg-runtime")
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let dbg_mgr = DbgManager::new().await;
            init_dbg_mgr(|| Arc::new(dbg_mgr));
            match get_dbg_mgr().start().await {
                Ok(_) => {
                    debug!("DbgManager started successfully.");
                    get_rt_status().up(Component::DbgMgr);
                }
                Err(e) => {
                    error!("Failed to start DbgManager: {:?}", e);
                    get_shutdown_ctrl()
                        .trigger_once(ShutdownCause::DbgMgrInitFailure);
                }
            }
            ShutdownCtrl::wait_for_exit().await;
            get_shutdown_ctrl()
                .shutdown_cleanup(async {
                    get_dbg_mgr().cleanup().await;
                })
                .await;
            get_shutdown_ctrl().ack_shutdown(Component::DbgMgr);
        });
    });

    // schedule cmd loop in the same thread
    // the main thread is sit idle and wait for the signal to stop
    let main_loop = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("ddb-cmd-loop")
            .build()
            .unwrap();
        rt.block_on(async {
            run_cmd_loop(get_shutdown_ctrl().subscribe()).await;
        });
        // Seems like tokio has trouble shutting down the IO reader properly
        // so we need to manually shutdown the runtime
        rt.shutdown_background();
    });

    get_shutdown_ctrl().wait_for_shutdown();

    app.join();
    main_loop.join().unwrap();
    dbg_handle.join().unwrap();
    cmd_flow_handle.join().unwrap();

    debug!("Exiting due to {:?}", get_shutdown_ctrl().cause());
    info!("Bye!");
    Ok(())
}
