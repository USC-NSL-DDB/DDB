pub mod framework_adapter;
pub mod handler;
pub mod input;
pub mod output;
pub mod router;
pub mod tracker;

use std::sync::{Arc, OnceLock};
use thiserror::Error;

pub use output::*;
pub use tracker::*;

use input::CmdHandler;
use router::Router;

static CMD_HANDLER: OnceLock<Arc<CmdHandler>> = OnceLock::new();
static CMD_ROUTER: OnceLock<Arc<Router>> = OnceLock::new();
static CMD_TRACKER: OnceLock<Arc<Tracker>> = OnceLock::new();

#[inline]
fn get_tracker() -> &'static Arc<Tracker> {
    CMD_TRACKER.get_or_init(|| Tracker::new())
}

#[inline]
pub fn init_cmd_handler<F>(f: F)
where
    F: FnOnce() -> Arc<CmdHandler>,
{
    CMD_HANDLER.get_or_init(f);
}

#[inline]
pub fn get_cmd_handler() -> &'static Arc<CmdHandler> {
    CMD_HANDLER.get().expect("CmdHandler is not initialized.")
}

#[inline]
pub fn get_router() -> &'static Arc<Router> {
    CMD_ROUTER.get_or_init(|| Arc::new(Router::new()))
}

#[inline]
pub fn get_cmd_tracker() -> Arc<Tracker> {
    get_tracker().clone()
}

#[inline]
pub fn get_output_tx(id: u64) -> flume::Sender<SessionResponse> {
    get_cmd_tracker().register_output_tx(id)
}

#[derive(Debug, Error)]
pub enum GdbDataErr {
    #[error("Missing entry: {0}")]
    MissingEntry(String),
}
