//! Transport-independent implementation of the public DDB v2 contract.
//!
//! Network adapters translate framing and authentication into this module.
//! Debugger state remains owned by the existing domain services and
//! RuntimeModel; this layer owns only public identities, validation,
//! projections, bounded operation history, and event synchronization.

mod command_port;
mod context;
mod debugger_reads;
mod error;
mod ids;
mod journal;
mod mutations;
mod operation_store;
mod pagination;
mod projection;
mod resource_catalog;
mod runtime_events;
mod service;
mod target;
pub(crate) use command_port::ApplicationCommandPort;
pub(crate) use command_port::CommandPortError;
#[cfg(test)]
pub(crate) use command_port::NoopCommandPort;

pub(crate) use context::{timestamp_after, timestamp_now, PrincipalContext, RequestScope};
pub(crate) use error::ApplicationError;
pub(crate) use ids::{OpaqueIdRegistry, ResourceIdKind};
pub(crate) use journal::{
    StateChange, StateEventContext, StateJournal, StateJournalConfig, StateSubscription,
};
pub(crate) use operation_store::{OperationStore, OperationStoreConfig};
pub(crate) use pagination::PageCodec;
pub(crate) use projection::{collection_revision, ProjectionContext};
pub(crate) use resource_catalog::ResourceCatalog;
pub(crate) use service::{DdbApplicationConfig, DdbApplicationService};
pub(crate) use target::{ResolvedTarget, TargetPurpose, TargetResolver};
