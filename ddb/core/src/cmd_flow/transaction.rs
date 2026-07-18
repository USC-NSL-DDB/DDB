//! Exclusive command sequences spanning one or more sessions.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    cmd_flow::{get_router, session_runtime::SessionLease},
    state::{get_state_mgr, SessionRef},
};

#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("Session not found: {0}")]
    SessionNotFound(u64),
    #[error("Unable to acquire session {0}: {1}")]
    Acquire(u64, String),
}

/// A primary state reference paired with ordered exclusive runtime leases.
///
/// Normal commands hold shared permits through completion. Acquiring every
/// related session in ascending id order therefore drains earlier work,
/// prevents later interleaving, and avoids lock-order inversion.
pub struct SessionTransaction {
    primary_sid: u64,
    session: SessionRef,
    leases: BTreeMap<u64, SessionLease>,
}

impl SessionTransaction {
    pub fn session(&self) -> &SessionRef {
        &self.session
    }

    pub fn lease(&self) -> &SessionLease {
        self.leases
            .get(&self.primary_sid)
            .expect("primary session lease is always present")
    }

    pub fn lease_for(&self, sid: u64) -> Option<&SessionLease> {
        self.leases.get(&sid)
    }
}

pub async fn begin(sid: u64) -> Result<SessionTransaction, TransactionError> {
    begin_with_related(sid, std::iter::empty()).await
}

fn ordered_session_ids(primary_sid: u64, related: impl IntoIterator<Item = u64>) -> Vec<u64> {
    let mut session_ids = related.into_iter().collect::<BTreeSet<_>>();
    session_ids.insert(primary_sid);
    session_ids.into_iter().collect()
}

pub async fn begin_with_related(
    primary_sid: u64,
    related: impl IntoIterator<Item = u64>,
) -> Result<SessionTransaction, TransactionError> {
    let session = get_state_mgr()
        .session(primary_sid)
        .ok_or(TransactionError::SessionNotFound(primary_sid))?;
    let session_ids = ordered_session_ids(primary_sid, related);

    let mut handles = Vec::with_capacity(session_ids.len());
    for sid in session_ids {
        let handle = get_router()
            .session_handle(sid)
            .map_err(|error| TransactionError::Acquire(sid, error.to_string()))?;
        handles.push((sid, handle));
    }

    let mut leases = BTreeMap::new();
    for (sid, handle) in handles {
        let lease = handle
            .exclusive()
            .await
            .map_err(|error| TransactionError::Acquire(sid, error.to_string()))?;
        leases.insert(sid, lease);
    }

    Ok(SessionTransaction {
        primary_sid,
        session,
        leases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn related_sessions_are_deduplicated_and_ordered() {
        assert_eq!(ordered_session_ids(7, [9, 3, 7, 9]), vec![3, 7, 9]);
    }

    #[test]
    fn transaction_error_identifies_missing_session() {
        let error = TransactionError::SessionNotFound(42);
        assert_eq!(error.to_string(), "Session not found: 42");
    }
}
