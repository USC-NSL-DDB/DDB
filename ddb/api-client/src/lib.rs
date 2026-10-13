//! Public Rust client for the DDB API v2 contract.
//!
//! The client owns transport framing, typed errors, request controls,
//! pagination, operation polling, and resumable event delivery. It depends only
//! on public contract types and never imports DDB backend or runtime code.

mod error;
mod http;
mod output_sync;
mod projection;
mod stream;
mod sync;
mod trace;
#[cfg(test)]
mod trace_tests;

pub use error::{ClientError, Result};
pub use http::{ClientConfig, DdbClient, SourceCatalog};
pub use output_sync::{OutputSync, OutputSyncItem, OutputSyncOptions};
pub use projection::{ProjectionUpdate, StateProjection};
pub use stream::NdjsonStream;
pub use sync::{
    ProjectedStateSync, ProjectedStateSyncItem, StateSync, StateSyncItem, StateSyncOptions,
};
pub use trace::{RequestTrace, RequestTraceOutcome};

/// Canonical public request, response, resource, and event types.
pub use ddb_api_types::v2;
/// Canonical Protobuf well-known types referenced by public requests.
pub use ddb_api_types::wkt;
