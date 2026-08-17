//! Generated, versioned contract types for DDB frontend APIs.
//!
//! This crate contains wire DTOs only. Debugger behavior lives in DDB's
//! application and domain services. Regenerate these files with
//! `cargo run -p ddb-api-codegen -- generate` after editing `proto/`.

/// Well-known Protobuf types used by the public contract.
///
/// Keeping these exports stable prevents generated consumers from depending on
/// the implementation crate selected for each canonical ProtoJSON mapping.
pub mod wkt;

/// DDB API version 2 contracts.
#[allow(clippy::large_enum_variant)]
pub mod v2 {
    include!("generated/ddb.api.v2.rs");
    include!("generated/ddb.api.v2.serde.rs");
}

/// Canonical descriptor set used by reflection, compatibility checks, and SDK
/// generators. It deliberately excludes source information for reproducibility.
pub const V2_FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("../descriptor/ddb_api_v2_descriptor.bin");
