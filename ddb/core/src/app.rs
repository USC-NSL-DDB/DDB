use std::sync::Arc;

use tracing::{error, info};

use crate::{
    api::server::ApiServer,
    notification::NotificationManager,
    shutdown::{get_shutdown_ctrl, ShutdownCause},
    source::resolver::SourceResolver,
};

pub struct App {
    api_svr: Arc<ApiServer>,
}

impl App {
    pub fn new(
        port: u16,
        notifications: Arc<NotificationManager>,
        source_resolver: Arc<SourceResolver>,
    ) -> Self {
        let api_svr = Arc::new(ApiServer::new(
            format!("localhost:{port}"),
            notifications,
            source_resolver,
        ));
        App { api_svr }
    }

    pub async fn run(&self, shutdown_rx: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        if let Err(error) = self.api_svr.run(shutdown_rx).await {
            error!("Error running server: {}", error);
            get_shutdown_ctrl().trigger_once(ShutdownCause::ApiServerInitFailure);
            return Err(error.into());
        }
        info!("API server stopped");
        Ok(())
    }
}
