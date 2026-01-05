//! # Command Flow Module
//!
//! This module manages the lifecycle of GDB commands in the distributed debugging system.
//!
//! ## Module Boundaries
//!
//! ### Public API (`api`)
//! The primary entry point for external callers. Provides ergonomic builders:
//! - `send()` - Fire-and-forget commands (optionally with custom formatters via `.with()`)
//! - `send_and_return()` - Commands that await responses (returns raw `FinishedCmd` data)
//!
//! ### Internal Components
//! - **`input`**: Parses command tokens, extracts target information, builds `Command<F>` types
//! - **`handler`**: Per-command handlers that implement special routing or processing logic
//! - **`router`**: Routes commands to specific sessions/threads, manages broadcast operations
//! - **`output`**: Formatters that transform GDB/MI responses for different consumers
//! - **`tracker`**: Tracks outstanding commands and manages response aggregation
//! - **`framework_adapter`**: Framework-specific command adaptations
//!
//! ## Data Flow
//! ```text
//! External Caller
//!   ↓
//! api::send/send_and_return (parse prefix/args, inject tokens)
//!   ↓
//! Command<F> (with formatter and tokens)
//!   ↓
//! Router::send_to/send_to_ret (resolve target → sessions)
//!   ↓
//! Session channels (flume) → GDB instances
//!   ↓
//! Response aggregation (Tracker)
//!   ↓
//! Formatter (transform & format)
//!   ↓
//! STDOUT or returned FinishedCmd
//! ```
//!
//! ## Responsibilities
//! - **Parse**: Extract tokens, prefix, args from input
//! - **Build**: Construct internal Command types with formatters
//! - **Execute**: Route to appropriate GDB sessions via Router
//! - **Transform**: Apply formatters to responses
//! - **Track**: Manage command/response correlation via tokens
//!

pub mod api;
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

// Re-export facade API for convenient access
#[allow(unused_imports)]
pub use api::{send, send_and_return, Error as ApiError, Target};

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
// FIXME: make this private
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
