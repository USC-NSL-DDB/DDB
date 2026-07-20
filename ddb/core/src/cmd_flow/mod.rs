//! Session-oriented debugger command flow.
//!
//! The public API builds semantic execution requests. The router resolves a target
//! into a snapshot of session handles, and each session runtime owns command
//! admission, transport I/O, response correlation, timeouts, and ordered state
//! projection. No transport channels cross component boundaries.

pub mod api;
pub(crate) mod backtrace;
pub(crate) mod breakpoint;
pub(crate) mod decoder;
pub mod engine;
pub(crate) mod event;
pub(crate) mod event_publisher;
pub(crate) mod execution;
pub mod framework_adapter;
pub mod handler;
pub mod input;
pub mod outcome;
pub mod output;
pub(crate) mod query;
pub mod response;
pub mod router;
pub mod session_runtime;
pub(crate) mod transaction;

pub use outcome::*;
pub use output::*;
pub use response::*;
// Re-export facade API for convenient access
#[allow(unused_imports)]
pub use api::{command, Error as ApiError, Target};
