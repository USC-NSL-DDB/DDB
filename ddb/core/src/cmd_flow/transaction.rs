//! # Session Transaction API
//!
//! Provides mutex-like synchronization for GDB command sequences to prevent
//! command interleaving when executing atomic sequences of commands.
//!
//! ## Usage
//!
//! ```ignore
//! // Single session transaction
//! let tx = begin(sid).await?;
//! // Execute atomic command sequence...
//! // tx is automatically released when dropped
//!
//! // Multi-session transaction (locks acquired in sid order to prevent deadlock)
//! let tx = begin_multi(&[sid1, sid2]).await?;
//! // Execute atomic command sequence across multiple sessions...
//! ```

use tokio::sync::OwnedMutexGuard;

use crate::state::{get_state_mgr, SessionRef};

/// Error type for transaction operations
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("Session not found: {0}")]
    SessionNotFound(u64),
}

/// A transaction guard for a single session.
///
/// Holds the exclusive lock on a session's transaction mutex.
/// The lock is automatically released when this guard is dropped.
#[derive(Debug)]
pub struct SessionTransaction {
    /// The session ID this transaction is for
    pub sid: u64,
    /// The session reference
    session: SessionRef,
    /// The owned mutex guard - keeps the lock held
    _guard: OwnedMutexGuard<()>,
}

impl SessionTransaction {
    /// Get access to the session reference for this transaction
    #[inline]
    pub fn session(&self) -> &SessionRef {
        &self.session
    }
}

/// A transaction guard for multiple sessions.
///
/// Holds exclusive locks on multiple sessions' transaction mutexes.
/// Locks are acquired in ascending session ID order to prevent deadlocks.
/// All locks are automatically released when this guard is dropped.
#[derive(Debug)]
pub struct MultiSessionTransaction {
    /// Individual session transactions, sorted by sid
    transactions: Vec<SessionTransaction>,
}

impl MultiSessionTransaction {
    /// Get the number of sessions in this transaction
    #[inline]
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Check if this transaction is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    /// Get a session transaction by session ID
    #[inline]
    pub fn get(&self, sid: u64) -> Option<&SessionTransaction> {
        self.transactions.iter().find(|tx| tx.sid == sid)
    }

    /// Get all session transactions
    #[inline]
    pub fn all(&self) -> &[SessionTransaction] {
        &self.transactions
    }

    /// Get session references for all sessions in this transaction
    #[inline]
    pub fn sessions(&self) -> Vec<&SessionRef> {
        self.transactions.iter().map(|tx| &tx.session).collect()
    }
}

/// Begin a transaction for a single session.
///
/// This acquires the session's transaction lock, providing exclusive access
/// for a sequence of commands. The lock is released when the returned
/// `SessionTransaction` is dropped.
///
/// # Arguments
/// * `sid` - The session ID to acquire the transaction lock for
///
/// # Returns
/// * `Ok(SessionTransaction)` - The transaction guard holding the lock
/// * `Err(TransactionError::SessionNotFound)` - If the session doesn't exist
///
/// # Example
/// ```ignore
/// let tx = begin(1).await?;
/// // Execute commands atomically...
/// // Lock released when tx goes out of scope
/// ```
pub async fn begin(sid: u64) -> Result<SessionTransaction, TransactionError> {
    let session = get_state_mgr()
        .session(sid)
        .ok_or(TransactionError::SessionNotFound(sid))?;

    let guard = session.lock_transaction_owned().await;

    Ok(SessionTransaction {
        sid,
        session,
        _guard: guard,
    })
}

/// Begin a transaction for multiple sessions.
///
/// This acquires transaction locks for all specified sessions in ascending
/// session ID order to prevent deadlocks. The locks are released when the
/// returned `MultiSessionTransaction` is dropped.
///
/// # Arguments
/// * `sids` - Slice of session IDs to acquire transaction locks for
///
/// # Returns
/// * `Ok(MultiSessionTransaction)` - The transaction guard holding all locks
/// * `Err(TransactionError::SessionNotFound)` - If any session doesn't exist
///
/// # Deadlock Prevention
/// Locks are always acquired in ascending session ID order, regardless of
/// the order in the input slice. This ensures consistent lock ordering
/// across all callers.
///
/// # Example
/// ```ignore
/// let tx = begin_multi(&[3, 1, 2]).await?;
/// // Locks acquired in order: 1, 2, 3
/// // Execute commands atomically across all sessions...
/// // All locks released when tx goes out of scope
/// ```
pub async fn begin_multi(sids: &[u64]) -> Result<MultiSessionTransaction, TransactionError> {
    if sids.is_empty() {
        return Ok(MultiSessionTransaction {
            transactions: Vec::new(),
        });
    }

    // Sort sids to ensure consistent lock ordering (prevents deadlock)
    let mut sorted_sids: Vec<u64> = sids.to_vec();
    sorted_sids.sort_unstable();
    sorted_sids.dedup(); // Remove duplicates

    let mut transactions = Vec::with_capacity(sorted_sids.len());

    // Acquire locks in order
    for sid in sorted_sids {
        let tx = begin(sid).await?;
        transactions.push(tx);
    }

    Ok(MultiSessionTransaction { transactions })
}

/// Try to begin a transaction for a single session without blocking.
///
/// This attempts to acquire the session's transaction lock immediately.
/// If the lock is already held, returns `None` instead of waiting.
///
/// # Arguments
/// * `sid` - The session ID to acquire the transaction lock for
///
/// # Returns
/// * `Ok(Some(SessionTransaction))` - The transaction guard if lock was acquired
/// * `Ok(None)` - If the lock is already held by another transaction
/// * `Err(TransactionError::SessionNotFound)` - If the session doesn't exist
pub async fn try_begin(sid: u64) -> Result<Option<SessionTransaction>, TransactionError> {
    let session = get_state_mgr()
        .session(sid)
        .ok_or(TransactionError::SessionNotFound(sid))?;

    match session.try_lock_transaction_owned() {
        Ok(guard) => Ok(Some(SessionTransaction {
            sid,
            session,
            _guard: guard,
        })),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require the state manager to be initialized.
    // They would typically be integration tests rather than unit tests.

    #[test]
    fn test_transaction_error_display() {
        let err = TransactionError::SessionNotFound(42);
        assert_eq!(err.to_string(), "Session not found: 42");
    }
}
