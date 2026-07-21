//! Field names of the projected query wire schema.
//!
//! The projector rewrites these fields with global identifiers and the
//! presenters aggregate them; both sides must agree, so the names have
//! exactly one definition.

pub(crate) const THREADS: &str = "threads";
pub(crate) const CURRENT_THREAD_ID: &str = "current-thread-id";
pub(crate) const GROUPS: &str = "groups";
pub(crate) const RECORD_ID: &str = "id";
pub(crate) const PROCESS_TYPE: &str = "type";
pub(crate) const PROCESS_PID: &str = "pid";
pub(crate) const PROCESS_EXECUTABLE: &str = "executable";
pub(crate) const PROCESS_DESC: &str = "desc";
