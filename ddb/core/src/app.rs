use std::sync::Arc;
use tracing::{error, info};

use crate::api::server::ApiServer;
use crate::shutdown::get_shutdown_ctrl;
use crate::status::{Component};

pub struct App {
    api_svr: Arc<ApiServer>,
    api_svr_handle: Option<std::thread::JoinHandle<()>>,
}

impl Default for App {
    fn default() -> Self {
        App::new(5000)
    }
}

impl App {
    pub fn new(port: u16) -> Self {
        let api_svr = Arc::new(ApiServer::new(format!("localhost:{}", port).as_str()));
        App {
            api_svr,
            api_svr_handle: None,
        }
    }

    pub fn run(&mut self, shutdown_rx: tokio::sync::watch::Receiver<bool>) {
        let server = Arc::clone(&self.api_svr);
        self.api_svr_handle = Some(std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                if let Err(error) = server.run(shutdown_rx).await {
                    error!("Error running server: {}", error);
                }
                info!("API server stopped");
                get_shutdown_ctrl().ack_shutdown(Component::Api);
            });
        }));
    }

    pub fn join(&mut self) {
        if let Some(handle) = self.api_svr_handle.take() {
            let _ = handle.join();
        }
    }
}
