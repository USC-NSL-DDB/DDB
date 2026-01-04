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
mod state;
mod status;

use std::sync::Arc;
use std::sync::OnceLock;

use app::App;
use clap::Parser;
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
use status::*;

use anyhow::Result;
use console_subscriber;
use nix::sys::signal::{pthread_sigmask, SigSet, SigmaskHow, Signal};
use rust_embed::Embed;
use tokio::io::{self, AsyncBufReadExt};
use tokio::time::timeout;
use tracing::debug;
use tracing::{info, warn};

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
    status::get_rt_status().wait_for_up().await;

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
                            SHUTDOWN_SIGNAL.trigger_once(status::ShutdownCause::UserExit);
                            println!("Exiting command loop...");
                            break;
                        }
                        get_cmd_handler().input(input).await;
                    }
                    Ok(None) => {
                        SHUTDOWN_SIGNAL.trigger_once(status::ShutdownCause::StdinEof);
                        println!("EOF reached, exiting command loop...");
                        break;
                    }
                    Err(err) => {
                        SHUTDOWN_SIGNAL.trigger_once(status::ShutdownCause::StdinError);
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
    // FIXME: we can remove this unsafe block with
    // OnceLock or lazy_static
    let logging_settings = LoggingSettings::from_args(&args);
    unsafe {
        Config::init_global(args.config)?;
    }
    let app_dir_conf = AppDirConfig::from_config(Config::global());

    // Keep the guard to ensure the async logger is running.
    let _guard = SetupProcedure::new()
        .with_app_dir_config(app_dir_conf)
        .with_logging_settings(logging_settings)
        .run()?;

    // register shutdown acks up front to avoid races
    let mut pending = vec![
        (
            Component::CmdFlow,
            SHUTDOWN_ACKS.register(Component::CmdFlow),
        ),
        (Component::DbgMgr, SHUTDOWN_ACKS.register(Component::DbgMgr)),
        (Component::Api, SHUTDOWN_ACKS.register(Component::Api)),
    ];

    // block signals in this thread (propagates to children); dedicated signal thread will wait on them
    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGINT);
    sigset.add(Signal::SIGTERM);
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&sigset), None).expect("failed to block signals");

    // start dedicated signal waiter thread
    let sigset_for_thread = sigset.clone();
    let signal_thread = std::thread::spawn(move || loop {
        match sigset_for_thread.wait() {
            Ok(Signal::SIGINT) => {
                SHUTDOWN_SIGNAL.trigger_once(status::ShutdownCause::SigInt);
                break;
            }
            Ok(Signal::SIGTERM) => {
                SHUTDOWN_SIGNAL.trigger_once(status::ShutdownCause::SigTerm);
                break;
            }
            Ok(_) => continue,
            Err(_) => {
                SHUTDOWN_SIGNAL.trigger_once(status::ShutdownCause::Other);
                break;
            }
        }
    });

    let mut app = App::new(Config::global().conf.api_server_port);
    app.run(SHUTDOWN_SIGNAL.subscribe());

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

            wait_for_exit().await;
            let _ = timeout(SHUTDOWN_TIMEOUT, async {
                cmd_handler.stop();
                tracker.stop();
            })
            .await;
            SHUTDOWN_ACKS.ack(Component::CmdFlow);
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
            get_dbg_mgr().start().await;

            get_rt_status().up(Component::DbgMgr);

            wait_for_exit().await;
            let _ = timeout(SHUTDOWN_TIMEOUT, async {
                get_dbg_mgr().cleanup().await;
            })
            .await;
            SHUTDOWN_ACKS.ack(Component::DbgMgr);
        });
    });

    // schedule cmd loop in the same thread
    // the main thread is sit idle and wait for the signal to stop
    let main_loop = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("ddb-main-loop")
            .build()
            .unwrap();
        rt.block_on(async {
            let cmd_loop_handler = tokio::spawn(run_cmd_loop(SHUTDOWN_SIGNAL.subscribe()));

            tokio::select! {
                _ = cmd_loop_handler => {}
                _ = wait_for_exit() => {}
            }
        });
        // Seems like tokio has trouble shutting down the IO reader properly
        // so we need to manually shutdown the runtime
        rt.shutdown_background();
    });

    // wait for all components to ack or timeout using a small runtime
    let wait_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    wait_rt.block_on(async {
        for (comp, rx) in pending.drain(..) {
            let res = timeout(SHUTDOWN_TIMEOUT, rx).await;
            match res {
                Ok(Ok(())) => {}
                Ok(Err(_)) => warn!("Shutdown ack dropped for {:?}", comp),
                Err(_) => warn!("Timed out waiting for shutdown ack from {:?}", comp),
            }
        }
    });

    app.join();
    main_loop.join().unwrap();
    dbg_handle.join().unwrap();
    cmd_flow_handle.join().unwrap();
    let _ = signal_thread.join();

    debug!("Exiting due to {:?}", SHUTDOWN_SIGNAL.cause());
    info!("Bye!");
    Ok(())
}
