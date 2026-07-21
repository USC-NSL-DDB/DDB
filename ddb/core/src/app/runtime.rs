//! Owns the application lifecycle: builds the service graph, supervises the
//! component tasks, and drives them to shutdown.

use std::sync::Arc;

use anyhow::Result;
use tokio::task::JoinSet;
use tracing::{debug, error};

use crate::{
    api::server::ApiServer,
    common::Config,
    dbg_mgr::DbgManager,
    debugger::DebuggerBackend,
    notification::NotificationManager,
    plugin::FrameworkPlugin,
    shutdown::{ShutdownCause, ShutdownCtrl},
    status::{Component, RuntimeStatus},
};

use super::{repl, ApplicationServices};

/// Owns the service graph and lifecycle state for one DDB instance.
pub(crate) struct ApplicationRuntime {
    services: Arc<ApplicationServices>,
    shutdown: Arc<ShutdownCtrl>,
    status: Arc<RuntimeStatus>,
}

impl ApplicationRuntime {
    pub(crate) async fn new(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
    ) -> Result<Self> {
        let shutdown = Arc::new(ShutdownCtrl::new());
        let status = Arc::new(RuntimeStatus::new());
        let services =
            ApplicationServices::build(config, plugin, backend, Arc::clone(&shutdown)).await?;
        Ok(Self {
            services,
            shutdown,
            status,
        })
    }

    #[cfg(test)]
    pub(crate) fn services(&self) -> &Arc<ApplicationServices> {
        &self.services
    }

    pub(crate) fn shutdown(&self) -> &Arc<ShutdownCtrl> {
        &self.shutdown
    }

    /// Runs every component to completion, escalating unexpected stops and
    /// failures into a shutdown of the whole runtime.
    pub(crate) async fn run(&self, command_workers: usize) -> Result<()> {
        let services = &self.services;
        let shutdown = &self.shutdown;
        let status = &self.status;
        let api_server = Arc::new(ApiServer::new(
            format!("localhost:{}", services.config().conf.api_server_port),
            Arc::clone(services.notification_manager()),
            Arc::clone(services.command_engine()),
            Arc::clone(services.api_queries()),
            Arc::clone(status),
        ));
        let mut tasks = JoinSet::new();

        {
            let status = Arc::clone(status);
            let shutdown = Arc::clone(shutdown);
            tasks.spawn(async move { ("command-flow", run_command_flow(status, shutdown).await) });
        }
        {
            let manager = Arc::clone(services.debugger_manager());
            let status = Arc::clone(status);
            let shutdown = Arc::clone(shutdown);
            tasks.spawn(async move {
                (
                    "debugger-manager",
                    run_debugger_manager(manager, status, shutdown).await,
                )
            });
        }
        {
            let manager = Arc::clone(services.notification_manager());
            let status = Arc::clone(status);
            let shutdown = Arc::clone(shutdown);
            tasks.spawn(async move {
                (
                    "notification-manager",
                    run_notification_manager(manager, status, shutdown).await,
                )
            });
        }
        {
            let shutdown = Arc::clone(shutdown);
            let stop = shutdown.subscribe();
            tasks.spawn(async move {
                (
                    "api-server",
                    run_api_server(api_server, shutdown, stop).await,
                )
            });
        }
        {
            let shutdown = Arc::clone(shutdown);
            tasks.spawn(async move {
                shutdown.wait_for_signal().await;
                ("signal-handler", Ok(()))
            });
        }
        {
            let engine = Arc::clone(services.command_engine());
            let status = Arc::clone(status);
            let shutdown_for_loop = Arc::clone(shutdown);
            let stop = shutdown.subscribe();
            tasks.spawn(async move {
                ("command-loop", {
                    repl::run(engine, command_workers, status, shutdown_for_loop, stop).await;
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

async fn run_api_server(
    api_server: Arc<ApiServer>,
    shutdown: Arc<ShutdownCtrl>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if let Err(error) = api_server.run(stop).await {
        error!("Error running server: {}", error);
        shutdown.trigger_once(ShutdownCause::ApiServerInitFailure);
        return Err(error.into());
    }
    tracing::info!("API server stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_state_is_owned_per_runtime() {
        let first_shutdown = ShutdownCtrl::new();
        let second_shutdown = ShutdownCtrl::new();

        first_shutdown.trigger_once(crate::shutdown::ShutdownCause::UserExit);

        assert!(first_shutdown.should_shutdown());
        assert!(!second_shutdown.should_shutdown());
    }

    #[tokio::test]
    async fn complete_service_graphs_do_not_share_runtime_state() {
        let config = Arc::new(Config::default());
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        let backend = crate::debugger::resolve_debugger_backend(config.as_ref()).unwrap();

        let first = ApplicationRuntime::new(
            Arc::clone(&config),
            Arc::clone(&plugin),
            Arc::clone(&backend),
        )
        .await
        .unwrap();
        let second = ApplicationRuntime::new(config, plugin, backend)
            .await
            .unwrap();

        assert!(!Arc::ptr_eq(
            first.services().api_queries().model(),
            second.services().api_queries().model(),
        ));

        first
            .shutdown()
            .trigger_once(crate::shutdown::ShutdownCause::UserExit);
        assert!(first.shutdown().should_shutdown());
        assert!(!second.shutdown().should_shutdown());
    }
}
