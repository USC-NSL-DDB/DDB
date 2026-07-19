//! Session-oriented debugger command flow.
//!
//! The public API builds semantic execution requests. The router resolves a target
//! into a snapshot of session handles, and each session runtime owns command
//! admission, transport I/O, response correlation, timeouts, and ordered state
//! projection. No transport channels cross component boundaries.

pub mod api;
pub mod engine;
pub(crate) mod event;
pub(crate) mod event_publisher;
pub mod framework_adapter;
pub mod handler;
pub mod input;
pub mod outcome;
pub mod output;
pub mod response;
pub mod router;
pub mod session_runtime;
pub(crate) mod transaction;

use std::sync::Arc;
use thiserror::Error;

pub use outcome::*;
pub use output::*;
pub use response::*;
// Re-export facade API for convenient access
#[allow(unused_imports)]
pub use api::{command, Error as ApiError, Target};

use engine::CommandEngine;
use router::Router;

#[inline]
pub fn get_command_engine() -> &'static Arc<CommandEngine> {
    crate::context::app_context().command_engine()
}

#[inline]
// FIXME: make this private
pub fn get_router() -> &'static Arc<Router> {
    crate::context::app_context().command_router()
}

#[derive(Debug, Error)]
pub enum DebuggerDataErr {
    #[error("Missing entry: {0}")]
    MissingEntry(String),
}
