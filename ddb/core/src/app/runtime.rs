//! Owns the application lifecycle: builds the service graph, supervises the
//! component tasks, and drives them to shutdown.

use std::{fs, sync::Arc};

use anyhow::{Context, Result};
use tokio::task::JoinSet;
use tracing::{debug, error};

use crate::{
    api::{
        auth::ApiAuthorization, security::validate_api_deployment_with_options, server::ApiServer,
    },
    common::Config,
    dbg_mgr::DbgManager,
    debugger::DebuggerBackend,
    notification::NotificationManager,
    plugin::FrameworkPlugin,
    shutdown::{ShutdownCause, ShutdownCtrl},
    startup::StartupReporter,
    status::{Component, RuntimeStatus},
};

#[cfg(feature = "grpc-preview")]
use crate::api::grpc::GrpcPreviewServer;

use super::{repl, ApplicationServices};

/// Owns the service graph and lifecycle state for one DDB instance.
pub(crate) struct ApplicationRuntime {
    services: Arc<ApplicationServices>,
    shutdown: Arc<ShutdownCtrl>,
    status: Arc<RuntimeStatus>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RuntimeConstructionOptions {
    pub allow_ephemeral_api_port: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeRunOptions {
    pub interactive: bool,
    pub command_workers: usize,
    pub startup_reporter: Option<StartupReporter>,
    pub remove_auth_token_after_load: bool,
}

impl ApplicationRuntime {
    #[cfg(test)]
    pub(crate) async fn new(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
    ) -> Result<Self> {
        Self::new_with_options(
            config,
            plugin,
            backend,
            RuntimeConstructionOptions::default(),
        )
        .await
    }

    pub(crate) async fn new_with_options(
        config: Arc<Config>,
        plugin: Arc<dyn FrameworkPlugin>,
        backend: Arc<dyn DebuggerBackend>,
        options: RuntimeConstructionOptions,
    ) -> Result<Self> {
        validate_api_deployment_with_options(config.as_ref(), options.allow_ephemeral_api_port)?;
        #[cfg(not(feature = "grpc-preview"))]
        if config.conf.api_grpc_preview_port.is_some() {
            anyhow::bail!(
                "Conf.api_grpc_preview_port requires DDB to be built with the grpc-preview feature"
            );
        }
        #[cfg(feature = "grpc-preview")]
        if let Some(port) = config.conf.api_grpc_preview_port {
            if port == 0 {
                anyhow::bail!("Conf.api_grpc_preview_port must be a non-zero port");
            }
            if port == config.conf.api_server_port {
                anyhow::bail!("Conf.api_grpc_preview_port must differ from Conf.api_server_port");
            }
        }
        let shutdown = Arc::new(ShutdownCtrl::new());
        let status = Arc::new(RuntimeStatus::new());
        let services = ApplicationServices::build(
            config,
            plugin,
            backend,
            Arc::clone(&shutdown),
            Arc::clone(&status),
        )
        .await?;
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
    pub(crate) async fn run_with_options(&self, options: RuntimeRunOptions) -> Result<()> {
        let services = &self.services;
        let shutdown = &self.shutdown;
        let status = &self.status;
        if let Some(reporter) = &options.startup_reporter {
            reporter.set_phase("auth_setup");
        }
        let authorization = ApiAuthorization::from_config(services.config().as_ref())?;
        if options.remove_auth_token_after_load {
            let path = services
                .config()
                .conf
                .api_auth_token_file
                .as_deref()
                .context("managed DDB omitted its API credential path")?;
            fs::remove_file(path)
                .with_context(|| format!("failed to unlink managed API credential {path}"))?;
        }
        let api_server = Arc::new(ApiServer::new(
            (
                services.config().conf.api_server_bind,
                services.config().conf.api_server_port,
            )
                .into(),
            Arc::clone(services.notification_manager()),
            Arc::clone(services.command_engine()),
            Arc::clone(services.api_queries()),
            Arc::clone(services.application_api()),
            Arc::clone(status),
            Arc::clone(services.config()),
            Arc::clone(shutdown),
            Arc::clone(&authorization),
        )?);
        if let Some(reporter) = &options.startup_reporter {
            reporter.set_phase("api_bind");
        }
        let api_listener = api_server
            .bind()
            .await
            .context("failed to bind the HTTP API listener")?;
        let api_addr = api_listener
            .local_addr()
            .context("failed to inspect API listener")?;

        #[cfg(feature = "grpc-preview")]
        let grpc_server = services.config().conf.api_grpc_preview_port.map(|port| {
            Arc::new(GrpcPreviewServer::new(
                ([127, 0, 0, 1], port).into(),
                Arc::clone(services.application_api()),
                Arc::clone(&authorization),
            ))
        });
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
                    run_api_server(api_server, api_listener, shutdown, stop).await,
                )
            });
        }
        #[cfg(feature = "grpc-preview")]
        if let Some(grpc_server) = grpc_server {
            let shutdown = Arc::clone(shutdown);
            let stop = shutdown.subscribe();
            tasks.spawn(async move {
                (
                    "grpc-preview-server",
                    run_grpc_server(grpc_server, shutdown, stop).await,
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
        if options.interactive {
            let engine = Arc::clone(services.command_engine());
            let status = Arc::clone(status);
            let shutdown_for_loop = Arc::clone(shutdown);
            let stop = shutdown.subscribe();
            tasks.spawn(async move {
                ("command-loop", {
                    repl::run(
                        engine,
                        options.command_workers,
                        status,
                        shutdown_for_loop,
                        stop,
                    )
                    .await;
                    Ok(())
                })
            });
        }

        if let Some(reporter) = &options.startup_reporter {
            reporter.set_phase("service_startup");
            let readiness = tokio::select! {
                _ = status.wait_for_up() => {
                    reporter.ready(api_addr, services.application_api().server_instance_id())
                }
                _ = shutdown.wait_for_exit() => {
                    Err(anyhow::anyhow!("DDB stopped before the API service became ready"))
                }
            };
            if let Err(error) = readiness {
                shutdown.trigger_once(ShutdownCause::Other);
                while tasks.join_next().await.is_some() {}
                return Err(error);
            }
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
    listener: tokio::net::TcpListener,
    shutdown: Arc<ShutdownCtrl>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if let Err(error) = api_server.run_listener(listener, stop).await {
        error!("Error running server: {}", error);
        shutdown.trigger_once(ShutdownCause::ApiServerInitFailure);
        return Err(error.into());
    }
    tracing::info!("API server stopped");
    Ok(())
}

#[cfg(feature = "grpc-preview")]
async fn run_grpc_server(
    grpc_server: Arc<GrpcPreviewServer>,
    shutdown: Arc<ShutdownCtrl>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if let Err(error) = grpc_server.run(stop).await {
        error!("Error running native gRPC preview server: {error}");
        shutdown.trigger_once(ShutdownCause::ApiServerInitFailure);
        return Err(error);
    }
    tracing::info!("Native gRPC preview server stopped");
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

    #[cfg(not(feature = "grpc-preview"))]
    #[tokio::test]
    async fn grpc_preview_configuration_fails_when_support_is_not_compiled() {
        let mut config = Config::default();
        config.conf.api_grpc_preview_port = Some(50051);
        let config = Arc::new(config);
        let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
        let backend = crate::debugger::resolve_debugger_backend(config.as_ref()).unwrap();

        let error = ApplicationRuntime::new(config, plugin, backend)
            .await
            .err()
            .expect("feature-disabled builds must reject gRPC preview configuration");
        assert!(error.to_string().contains("grpc-preview feature"));
    }

    #[cfg(feature = "grpc-preview")]
    #[tokio::test]
    async fn grpc_preview_configuration_rejects_zero_and_http_port_collisions() {
        for grpc_port in [0, Config::default().conf.api_server_port] {
            let mut config = Config::default();
            config.conf.api_grpc_preview_port = Some(grpc_port);
            let config = Arc::new(config);
            let plugin = crate::plugin::resolve_framework_plugin(config.as_ref());
            let backend = crate::debugger::resolve_debugger_backend(config.as_ref()).unwrap();

            assert!(
                ApplicationRuntime::new(config, plugin, backend)
                    .await
                    .is_err(),
                "gRPC preview port {grpc_port} should be rejected"
            );
        }
    }
}
