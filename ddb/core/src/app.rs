use std::sync::Arc;
use tracing::error;

use crate::api::server::ApiServer;

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

    pub fn run(&mut self) {
        let server = Arc::clone(&self.api_svr);
        self.api_svr_handle = Some(std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                if let Err(error) = server.run().await {
                    error!("Error running server: {}", error);
                }
            });
        }));
    }
}
