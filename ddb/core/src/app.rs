use std::sync::Arc;

use tracing::{error, info};

use crate::{
    api::server::ApiServer,
    context::ApplicationServices,
    shutdown::{ShutdownCause, ShutdownCtrl},
    status::RuntimeStatus,
};

pub struct App {
    api_svr: Arc<ApiServer>,
    shutdown: Arc<ShutdownCtrl>,
}

impl App {
    pub fn new(
        port: u16,
        services: &ApplicationServices,
        shutdown: Arc<ShutdownCtrl>,
        status: Arc<RuntimeStatus>,
    ) -> Self {
        let api_svr = Arc::new(ApiServer::new(
            format!("localhost:{port}"),
            Arc::clone(services.notification_manager()),
            Arc::clone(services.command_engine()),
            Arc::clone(services.api_queries()),
            status,
        ));
        App { api_svr, shutdown }
    }

    pub async fn run(&self, shutdown_rx: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        if let Err(error) = self.api_svr.run(shutdown_rx).await {
            error!("Error running server: {}", error);
            self.shutdown
                .trigger_once(ShutdownCause::ApiServerInitFailure);
            return Err(error.into());
        }
        info!("API server stopped");
        Ok(())
    }
}
