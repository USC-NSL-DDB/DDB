use std::sync::Arc;
use tracing::{error, info};

use crate::api::server::ApiServer;
use crate::shutdown::{get_shutdown_ctrl, ShutdownCause};

pub struct App {
    api_svr: Arc<ApiServer>,
}

impl Default for App {
    fn default() -> Self {
        App::new(5000)
    }
}

impl App {
    pub fn new(port: u16) -> Self {
        let api_svr = Arc::new(ApiServer::new(format!("localhost:{}", port).as_str()));
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
