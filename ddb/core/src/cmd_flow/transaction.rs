//! Scoped serialization for the few multi-command debugger operations that
//! must not interleave on one session.

use tokio::sync::OwnedMutexGuard;

use crate::state::{get_state_mgr, SessionRef};

#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("Session not found: {0}")]
    SessionNotFound(u64),
}

/// Exclusive access to one session for a multi-command sequence.
///
/// The guard is deliberately crate-private: ordinary commands go through the
/// router lifecycle, while only handlers that truly need a sequence lock use
/// this scope. Dropping it releases the session lock.
#[derive(Debug)]
pub struct SessionTransaction {
    session: SessionRef,
    _guard: OwnedMutexGuard<()>,
}

impl SessionTransaction {
    #[inline]
    pub fn session(&self) -> &SessionRef {
        &self.session
    }
}

pub async fn begin(sid: u64) -> Result<SessionTransaction, TransactionError> {
    let session = get_state_mgr()
        .session(sid)
        .ok_or(TransactionError::SessionNotFound(sid))?;
    let guard = session.lock_transaction_owned().await;

    Ok(SessionTransaction {
        session,
        _guard: guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_error_identifies_missing_session() {
        let error = TransactionError::SessionNotFound(42);
        assert_eq!(error.to_string(), "Session not found: 42");
    }
}
