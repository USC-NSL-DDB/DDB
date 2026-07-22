//! Exclusive command sequences spanning one or more sessions.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use crate::cmd_flow::{router::Router, session_runtime::SessionLease};
use crate::state::{RuntimeModel, SessionSnapshot, ThreadContext, ThreadStatus};

#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("Session not found: {0}")]
    SessionNotFound(u64),
    #[error("Unable to acquire session {0}: {1}")]
    Acquire(u64, String),
}

/// Ordered exclusive runtime leases paired with controlled model operations for
/// the primary session. No mutable session reference crosses this boundary.
pub struct SessionTransaction {
    primary_sid: u64,
    model: Arc<RuntimeModel>,
    leases: BTreeMap<u64, SessionLease>,
}

impl SessionTransaction {
    pub fn lease(&self) -> &SessionLease {
        self.leases
            .get(&self.primary_sid)
            .expect("primary session lease is always present")
    }

    pub fn lease_for(&self, sid: u64) -> Option<&SessionLease> {
        self.leases.get(&sid)
    }

    pub(crate) async fn session_snapshot(&self) -> Option<SessionSnapshot> {
        self.model.session_snapshot(self.primary_sid).await
    }

    pub(crate) async fn enter_custom_context(&self, context: ThreadContext) -> bool {
        self.model
            .enter_custom_context(self.primary_sid, context)
            .await
    }

    pub(crate) async fn finish_context_restore(&self, restored: bool) -> bool {
        self.model
            .finish_context_restore(self.primary_sid, restored)
            .await
    }

    pub(crate) async fn all_threads_stopped(&self) -> Option<bool> {
        self.model.all_threads_stopped(self.primary_sid).await
    }

    pub(crate) async fn mark_all_threads(
        &self,
        status: ThreadStatus,
    ) -> crate::state::StateTransitionResult<()> {
        self.model.mark_all_threads(self.primary_sid, status).await
    }
}

#[derive(Clone)]
pub(crate) struct TransactionCoordinator {
    model: Arc<RuntimeModel>,
    router: Arc<Router>,
}

impl TransactionCoordinator {
    pub(crate) fn new(model: Arc<RuntimeModel>, router: Arc<Router>) -> Self {
        Self { model, router }
    }

    pub(crate) async fn begin(
        &self,
        session_id: u64,
    ) -> Result<SessionTransaction, TransactionError> {
        self.begin_with_related(session_id, std::iter::empty())
            .await
    }

    pub(crate) async fn begin_with_related(
        &self,
        primary_session_id: u64,
        related: impl IntoIterator<Item = u64>,
    ) -> Result<SessionTransaction, TransactionError> {
        acquire_transaction(
            Arc::clone(&self.model),
            self.router.as_ref(),
            primary_session_id,
            related,
        )
        .await
    }
}

impl fmt::Debug for TransactionCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TransactionCoordinator").finish()
    }
}

fn ordered_session_ids(primary_sid: u64, related: impl IntoIterator<Item = u64>) -> Vec<u64> {
    let mut session_ids = related.into_iter().collect::<BTreeSet<_>>();
    session_ids.insert(primary_sid);
    session_ids.into_iter().collect()
}

async fn acquire_transaction(
    model: Arc<RuntimeModel>,
    router: &Router,
    primary_sid: u64,
    related: impl IntoIterator<Item = u64>,
) -> Result<SessionTransaction, TransactionError> {
    if model.session_snapshot(primary_sid).await.is_none() {
        return Err(TransactionError::SessionNotFound(primary_sid));
    }
    let session_ids = ordered_session_ids(primary_sid, related);

    let mut handles = Vec::with_capacity(session_ids.len());
    for sid in session_ids {
        let handle = router
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
        model,
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
