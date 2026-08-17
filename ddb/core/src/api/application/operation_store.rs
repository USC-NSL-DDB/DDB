use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use ddb_api_types::v2::{
    Cursor, DdbError, DdbErrorCode, Operation, OperationKind, OperationResult, OperationState,
    RequestContext, Target, TargetOutcome, TargetSummary,
};
use prost::Message;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{timestamp_after, timestamp_now, ApplicationError};
use crate::api::telemetry::{
    record_idempotent_replay, record_operation_store_depth, record_operation_transition,
};

#[derive(Clone, Debug)]
pub(crate) struct OperationStoreConfig {
    pub(crate) max_records: usize,
    pub(crate) max_bytes: usize,
    pub(crate) max_record_bytes: usize,
    pub(crate) retention: Duration,
    pub(crate) max_idempotency_key_bytes: usize,
}

impl Default for OperationStoreConfig {
    fn default() -> Self {
        Self {
            max_records: 1_024,
            max_bytes: 64 * 1024 * 1024,
            max_record_bytes: 64 * 1024,
            retention: Duration::from_secs(15 * 60),
            max_idempotency_key_bytes: 256,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OperationAdmission {
    pub(crate) operation: Operation,
    pub(crate) newly_admitted: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct OperationWithContext {
    pub(crate) operation: Operation,
    pub(crate) session_ids: Vec<u64>,
    pub(crate) group_ids: Vec<u64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct IdempotencyScope {
    principal: String,
    key_reference: String,
}

#[derive(Clone)]
struct IdempotencyEntry {
    operation_id: String,
    request_fingerprint: [u8; 32],
}

struct StoredOperation {
    operation: Operation,
    session_ids: Vec<u64>,
    group_ids: Vec<u64>,
    idempotency_scope: IdempotencyScope,
    expires_at: Instant,
}

#[derive(Default)]
struct StoreState {
    operations: HashMap<String, StoredOperation>,
    order: VecDeque<String>,
    idempotency: HashMap<IdempotencyScope, IdempotencyEntry>,
    reserved_bytes: usize,
}

/// Bounded operation history and atomic idempotency admission.
///
/// Each operation owns a fixed byte reservation. Active operations are never
/// evicted, and raw idempotency keys or request payloads are not retained.
pub(crate) struct OperationStore {
    server_instance_id: String,
    config: OperationStoreConfig,
    state: Mutex<StoreState>,
}

impl OperationStore {
    pub(crate) fn new(server_instance_id: impl Into<String>, config: OperationStoreConfig) -> Self {
        assert!(config.max_records > 0);
        assert!(config.max_record_bytes > 0);
        assert!(config.max_record_bytes <= config.max_bytes);
        assert!(config.max_idempotency_key_bytes > 0);
        Self {
            server_instance_id: server_instance_id.into(),
            config,
            state: Mutex::new(StoreState::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, StoreState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn lookup_idempotent(
        &self,
        context: Option<&RequestContext>,
        principal_scope: &str,
        request_fingerprint: &[u8],
    ) -> Result<Option<Operation>, ApplicationError> {
        let idempotency_key =
            required_idempotency_key(context, self.config.max_idempotency_key_bytes)?;
        if principal_scope.is_empty() {
            return Err(ApplicationError::new(
                DdbErrorCode::Unauthenticated,
                "principal context is required for mutation admission",
            ));
        }
        let scope = IdempotencyScope {
            principal: principal_scope.to_string(),
            key_reference: self.key_reference(principal_scope, idempotency_key),
        };
        let request_fingerprint: [u8; 32] = Sha256::digest(request_fingerprint).into();
        let mut state = self.state();
        self.prune_expired(&mut state, Instant::now());
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        let Some(existing) = state.idempotency.get(&scope) else {
            return Ok(None);
        };
        if existing.request_fingerprint != request_fingerprint {
            return Err(ApplicationError::new(
                DdbErrorCode::Conflict,
                "idempotency key was already used for a different mutation",
            ));
        }
        Ok(Some(
            state
                .operations
                .get(&existing.operation_id)
                .expect("idempotency index must reference a retained operation")
                .operation
                .clone(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit(
        &self,
        context: Option<&RequestContext>,
        principal_scope: &str,
        retained_principal: Option<&str>,
        request_id: &str,
        kind: OperationKind,
        target: Target,
        resolved_target_count: u32,
        resolved_session_ids: &[u64],
        resolved_group_ids: &[u64],
        cancellable: bool,
        request_fingerprint: &[u8],
    ) -> Result<OperationAdmission, ApplicationError> {
        let idempotency_key =
            required_idempotency_key(context, self.config.max_idempotency_key_bytes)?;
        if principal_scope.is_empty() {
            return Err(ApplicationError::new(
                DdbErrorCode::Unauthenticated,
                "principal context is required for mutation admission",
            ));
        }

        let scope = IdempotencyScope {
            principal: principal_scope.to_string(),
            key_reference: self.key_reference(principal_scope, idempotency_key),
        };
        let request_fingerprint: [u8; 32] = Sha256::digest(request_fingerprint).into();
        let now = Instant::now();
        let mut state = self.state();
        self.prune_expired(&mut state, now);

        if let Some(existing) = state.idempotency.get(&scope) {
            if existing.request_fingerprint != request_fingerprint {
                return Err(ApplicationError::new(
                    DdbErrorCode::Conflict,
                    "idempotency key was already used for a different mutation",
                ));
            }
            let operation = state
                .operations
                .get(&existing.operation_id)
                .expect("idempotency index must reference a retained operation")
                .operation
                .clone();
            let kind =
                OperationKind::try_from(operation.kind).unwrap_or(OperationKind::Unspecified);
            drop(state);
            record_idempotent_replay(kind);
            return Ok(OperationAdmission {
                operation,
                newly_admitted: false,
            });
        }

        self.make_capacity(&mut state)?;
        let operation_id = format!("op_{}", Uuid::new_v4().simple());
        let operation = Operation {
            operation_id: operation_id.clone(),
            request_id: request_id.to_string(),
            kind: kind as i32,
            target: Some(TargetSummary {
                target: Some(target),
                resolved_target_count,
            }),
            idempotency_key_reference: Some(scope.key_reference.clone()),
            principal: retained_principal.map(str::to_string),
            state: OperationState::Accepted as i32,
            accepted_at: Some(timestamp_now()),
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            target_outcomes: Vec::new(),
            state_revision: None,
            state_event_cursor: None,
            cancellable,
            revision: 1,
            expires_at: Some(timestamp_after(self.config.retention)),
        };
        self.ensure_record_bound(&operation)?;

        state.idempotency.insert(
            scope.clone(),
            IdempotencyEntry {
                operation_id: operation_id.clone(),
                request_fingerprint,
            },
        );
        state.order.push_back(operation_id.clone());
        state.reserved_bytes += self.config.max_record_bytes;
        state.operations.insert(
            operation_id,
            StoredOperation {
                operation: operation.clone(),
                session_ids: resolved_session_ids.to_vec(),
                group_ids: resolved_group_ids.to_vec(),
                idempotency_scope: scope,
                expires_at: now + self.config.retention,
            },
        );
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        drop(state);
        record_operation_transition(&operation);
        Ok(OperationAdmission {
            operation,
            newly_admitted: true,
        })
    }

    pub(crate) fn get(&self, operation_id: &str) -> Result<Operation, ApplicationError> {
        if operation_id.is_empty() {
            return Err(ApplicationError::invalid(
                "operation_id",
                "must not be empty",
            ));
        }
        let mut state = self.state();
        self.prune_expired(&mut state, Instant::now());
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        state
            .operations
            .get(operation_id)
            .map(|stored| stored.operation.clone())
            .ok_or_else(|| ApplicationError::not_found("operation"))
    }

    pub(crate) fn target_context(
        &self,
        operation_id: &str,
    ) -> Result<(Vec<u64>, Vec<u64>), ApplicationError> {
        let mut state = self.state();
        self.prune_expired(&mut state, Instant::now());
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        state
            .operations
            .get(operation_id)
            .map(|stored| (stored.session_ids.clone(), stored.group_ids.clone()))
            .ok_or_else(|| ApplicationError::not_found("operation"))
    }

    #[cfg(test)]
    fn list(&self) -> Vec<Operation> {
        let mut state = self.state();
        self.prune_expired(&mut state, Instant::now());
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        state
            .order
            .iter()
            .filter_map(|id| state.operations.get(id))
            .map(|stored| stored.operation.clone())
            .collect()
    }

    pub(crate) fn list_with_context(&self) -> Vec<OperationWithContext> {
        let mut state = self.state();
        self.prune_expired(&mut state, Instant::now());
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        state
            .order
            .iter()
            .filter_map(|id| state.operations.get(id))
            .map(|stored| OperationWithContext {
                operation: stored.operation.clone(),
                session_ids: stored.session_ids.clone(),
                group_ids: stored.group_ids.clone(),
            })
            .collect()
    }

    pub(crate) fn mark_running(&self, operation_id: &str) -> Result<Operation, ApplicationError> {
        self.update(operation_id, OperationState::Running, |operation| {
            operation.started_at = Some(timestamp_now());
        })
    }

    pub(crate) fn complete(
        &self,
        operation_id: &str,
        result: Option<OperationResult>,
        target_outcomes: Vec<TargetOutcome>,
        state_revision: Option<u64>,
        cursor: Option<Cursor>,
    ) -> Result<Operation, ApplicationError> {
        self.update(operation_id, OperationState::Completed, |operation| {
            operation.result = result;
            operation.target_outcomes = target_outcomes;
            operation.state_revision = state_revision;
            operation.state_event_cursor = cursor;
            operation.cancellable = false;
        })
    }

    pub(crate) fn fail(
        &self,
        operation_id: &str,
        error: DdbError,
        target_outcomes: Vec<TargetOutcome>,
    ) -> Result<Operation, ApplicationError> {
        self.fail_with_result(operation_id, error, target_outcomes, None)
    }

    pub(crate) fn fail_with_result(
        &self,
        operation_id: &str,
        error: DdbError,
        target_outcomes: Vec<TargetOutcome>,
        result: Option<OperationResult>,
    ) -> Result<Operation, ApplicationError> {
        self.update(operation_id, OperationState::Failed, |operation| {
            operation.error = Some(error);
            operation.target_outcomes = target_outcomes;
            operation.result = result;
            operation.cancellable = false;
        })
    }

    pub(crate) fn cancel(&self, operation_id: &str) -> Result<Operation, ApplicationError> {
        let current = self.get(operation_id)?;
        if !current.cancellable {
            return Err(ApplicationError::new(
                DdbErrorCode::NotCancellable,
                "operation cannot be cancelled in its current state",
            )
            .with_operation_id(operation_id));
        }
        self.update(operation_id, OperationState::Cancelled, |operation| {
            operation.cancellable = false;
        })
    }

    fn update(
        &self,
        operation_id: &str,
        next: OperationState,
        mutate: impl FnOnce(&mut Operation),
    ) -> Result<Operation, ApplicationError> {
        let now = Instant::now();
        let mut state = self.state();
        self.prune_expired(&mut state, now);
        let stored = state
            .operations
            .get_mut(operation_id)
            .ok_or_else(|| ApplicationError::not_found("operation"))?;
        let current =
            OperationState::try_from(stored.operation.state).unwrap_or(OperationState::Unspecified);
        if !valid_transition(current, next) {
            return Err(ApplicationError::new(
                DdbErrorCode::Conflict,
                format!("operation cannot transition from {current:?} to {next:?}"),
            )
            .with_operation_id(operation_id));
        }

        let mut candidate = stored.operation.clone();
        candidate.state = next as i32;
        candidate.revision = candidate.revision.saturating_add(1);
        mutate(&mut candidate);
        if terminal(next) {
            candidate.completed_at = Some(timestamp_now());
            candidate.expires_at = Some(timestamp_after(self.config.retention));
        }
        self.ensure_record_bound(&candidate)?;

        stored.operation = candidate.clone();
        if terminal(next) {
            stored.expires_at = now + self.config.retention;
        }
        record_operation_store_depth(state.operations.len(), state.reserved_bytes);
        drop(state);
        record_operation_transition(&candidate);
        Ok(candidate)
    }

    fn make_capacity(&self, state: &mut StoreState) -> Result<(), ApplicationError> {
        while state.operations.len() >= self.config.max_records
            || state
                .reserved_bytes
                .saturating_add(self.config.max_record_bytes)
                > self.config.max_bytes
        {
            let evictable = state.order.iter().find_map(|id| {
                state
                    .operations
                    .get(id)
                    .filter(|stored| operation_is_terminal(&stored.operation))
                    .map(|_| id.clone())
            });
            let Some(evictable) = evictable else {
                return Err(ApplicationError::resource_exhausted(
                    "operation capacity is occupied by active work",
                )
                .retryable(true));
            };
            self.remove(state, &evictable);
        }
        Ok(())
    }

    fn prune_expired(&self, state: &mut StoreState, now: Instant) {
        let expired = state
            .operations
            .iter()
            .filter(|(_, stored)| {
                operation_is_terminal(&stored.operation) && stored.expires_at <= now
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            self.remove(state, &id);
        }
    }

    fn remove(&self, state: &mut StoreState, operation_id: &str) {
        if let Some(stored) = state.operations.remove(operation_id) {
            state.idempotency.remove(&stored.idempotency_scope);
            state.order.retain(|id| id != operation_id);
            state.reserved_bytes = state
                .reserved_bytes
                .saturating_sub(self.config.max_record_bytes);
        }
    }

    fn ensure_record_bound(&self, operation: &Operation) -> Result<(), ApplicationError> {
        if operation.encoded_len() > self.config.max_record_bytes {
            return Err(ApplicationError::resource_exhausted(
                "operation result exceeds the retained record byte limit",
            ));
        }
        Ok(())
    }

    fn key_reference(&self, principal: &str, raw_key: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(self.server_instance_id.as_bytes());
        digest.update([0]);
        digest.update(principal.as_bytes());
        digest.update([0]);
        digest.update(raw_key.as_bytes());
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn required_idempotency_key(
    context: Option<&RequestContext>,
    max_bytes: usize,
) -> Result<&str, ApplicationError> {
    let key = context
        .and_then(|context| context.idempotency_key.as_deref())
        .ok_or_else(|| {
            ApplicationError::invalid("context.idempotency_key", "is required for every mutation")
        })?;
    if key.trim().is_empty() {
        return Err(ApplicationError::invalid(
            "context.idempotency_key",
            "must not be empty",
        ));
    }
    if key.len() > max_bytes {
        return Err(ApplicationError::invalid(
            "context.idempotency_key",
            format!("must not exceed {max_bytes} UTF-8 bytes"),
        ));
    }
    Ok(key)
}

fn terminal(state: OperationState) -> bool {
    matches!(
        state,
        OperationState::Completed | OperationState::Failed | OperationState::Cancelled
    )
}

fn operation_is_terminal(operation: &Operation) -> bool {
    OperationState::try_from(operation.state).is_ok_and(terminal)
}

fn valid_transition(current: OperationState, next: OperationState) -> bool {
    matches!(
        (current, next),
        (OperationState::Accepted, OperationState::Running)
            | (OperationState::Accepted, OperationState::Completed)
            | (OperationState::Accepted, OperationState::Failed)
            | (OperationState::Accepted, OperationState::Cancelled)
            | (OperationState::Running, OperationState::Completed)
            | (OperationState::Running, OperationState::Failed)
            | (OperationState::Running, OperationState::Cancelled)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddb_api_types::v2::{target, BroadcastTarget};

    fn config(max_records: usize) -> OperationStoreConfig {
        OperationStoreConfig {
            max_records,
            max_bytes: max_records * 2_048,
            max_record_bytes: 2_048,
            retention: Duration::from_secs(60),
            max_idempotency_key_bytes: 32,
        }
    }

    fn context(key: &str) -> RequestContext {
        RequestContext {
            idempotency_key: Some(key.to_string()),
            ..RequestContext::default()
        }
    }

    fn target() -> Target {
        Target {
            selector: Some(target::Selector::Broadcast(BroadcastTarget {})),
        }
    }

    fn admit(
        store: &OperationStore,
        key: &str,
        fingerprint: &[u8],
    ) -> Result<OperationAdmission, ApplicationError> {
        let request_context = context(key);
        store.admit(
            Some(&request_context),
            "principal-a",
            Some("principal-a"),
            "request",
            OperationKind::Execute,
            target(),
            2,
            &[1, 2],
            &[],
            false,
            fingerprint,
        )
    }

    #[test]
    fn duplicate_request_is_admitted_once_and_different_payload_conflicts() {
        let store = OperationStore::new("instance", config(4));
        let first = admit(&store, "key", b"same").unwrap();
        let second = admit(&store, "key", b"same").unwrap();
        assert!(first.newly_admitted);
        assert!(!second.newly_admitted);
        assert_eq!(first.operation.operation_id, second.operation.operation_id);
        assert_eq!(
            admit(&store, "key", b"different").unwrap_err().code(),
            DdbErrorCode::Conflict
        );
        assert_ne!(
            first.operation.idempotency_key_reference.as_deref(),
            Some("key")
        );
    }

    #[test]
    fn active_operations_are_never_evicted() {
        let store = OperationStore::new("instance", config(1));
        admit(&store, "one", b"one").unwrap();
        assert_eq!(
            admit(&store, "two", b"two").unwrap_err().code(),
            DdbErrorCode::ResourceExhausted
        );
    }

    #[test]
    fn terminal_operations_are_evictable_and_transitions_are_checked() {
        let store = OperationStore::new("instance", config(1));
        let first = admit(&store, "one", b"one").unwrap().operation;
        store
            .complete(&first.operation_id, None, Vec::new(), None, None)
            .unwrap();
        assert!(admit(&store, "two", b"two").is_ok());
        assert_eq!(
            store.mark_running(&first.operation_id).unwrap_err().code(),
            DdbErrorCode::NotFound
        );

        let second = store.list().pop().unwrap();
        store.mark_running(&second.operation_id).unwrap();
        assert_eq!(
            store.mark_running(&second.operation_id).unwrap_err().code(),
            DdbErrorCode::Conflict
        );
    }

    #[test]
    fn mutation_requires_a_bounded_nonempty_key() {
        let store = OperationStore::new("instance", config(2));
        let error = store
            .admit(
                None,
                "principal",
                None,
                "request",
                OperationKind::Execute,
                target(),
                1,
                &[],
                &[],
                false,
                b"request",
            )
            .unwrap_err();
        assert_eq!(error.code(), DdbErrorCode::InvalidArgument);
        assert!(admit(&store, "", b"request").is_err());
    }
}
