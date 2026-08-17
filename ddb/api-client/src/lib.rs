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

pub use error::{ClientError, Result};
pub use http::{ClientConfig, DdbClient};
pub use output_sync::{OutputSync, OutputSyncItem, OutputSyncOptions};
pub use projection::{ProjectionUpdate, StateProjection};
pub use stream::NdjsonStream;
pub use sync::{
    ProjectedStateSync, ProjectedStateSyncItem, StateSync, StateSyncItem, StateSyncOptions,
};

/// Canonical public request, response, resource, and event types.
pub use ddb_api_types::v2;
/// Canonical Protobuf well-known types referenced by public requests.
pub use ddb_api_types::wkt;
