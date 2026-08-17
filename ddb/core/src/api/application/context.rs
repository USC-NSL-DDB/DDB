use std::{
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ddb_api_types::{
    v2::{PermissionScope, RequestContext, ResponseContext},
    wkt::Timestamp,
};
use uuid::Uuid;

use super::ApplicationError;

const MIN_PROTO_SECONDS: i64 = -62_135_596_800;
const MAX_PROTO_SECONDS: i64 = 253_402_300_799;

/// Authenticated caller identity supplied by a transport adapter. The value is
/// a stable non-secret reference and scopes idempotency records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrincipalContext {
    id: String,
    scope: PermissionScope,
}

impl PrincipalContext {
    /// Construct a trusted in-process principal. Transport adapters must use
    /// `with_scope` with the authenticated grant.
    #[cfg(test)]
    pub(crate) fn new(id: impl Into<String>) -> Result<Self, ApplicationError> {
        Self::with_scope(id, PermissionScope::Admin)
    }

    pub(crate) fn with_scope(
        id: impl Into<String>,
        scope: PermissionScope,
    ) -> Result<Self, ApplicationError> {
        let id = id.into();
        if id.trim().is_empty() || id.len() > 256 {
            return Err(ApplicationError::new(
                ddb_api_types::v2::DdbErrorCode::Unauthenticated,
                "authenticated principal identity is invalid",
            ));
        }
        if scope == PermissionScope::Unspecified {
            return Err(ApplicationError::new(
                ddb_api_types::v2::DdbErrorCode::Unauthenticated,
                "authenticated principal scope is invalid",
            ));
        }
        Ok(Self { id, scope })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn allows(&self, required: PermissionScope) -> bool {
        match self.scope {
            PermissionScope::Admin => true,
            PermissionScope::Control => {
                matches!(required, PermissionScope::Read | PermissionScope::Control)
            }
            PermissionScope::Read => required == PermissionScope::Read,
            PermissionScope::Unspecified => false,
        }
    }
}

pub(crate) struct RequestScope {
    request_id: String,
    deadline: Option<Timestamp>,
}

impl RequestScope {
    pub(crate) fn begin(context: Option<&RequestContext>) -> Result<Self, ApplicationError> {
        let deadline = context.and_then(|context| context.deadline);
        if let Some(deadline) = deadline.as_ref() {
            validate_timestamp(deadline, "context.deadline")?;
            if timestamp_has_elapsed(deadline, &timestamp_now()) {
                return Err(ApplicationError::deadline_exceeded());
            }
        }
        Ok(Self {
            request_id: Uuid::new_v4().to_string(),
            deadline,
        })
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn ensure_active(&self) -> Result<(), ApplicationError> {
        if self
            .deadline
            .as_ref()
            .is_some_and(|deadline| timestamp_has_elapsed(deadline, &timestamp_now()))
        {
            Err(ApplicationError::deadline_exceeded())
        } else {
            Ok(())
        }
    }

    pub(crate) async fn wait<F>(&self, future: F) -> Result<F::Output, ApplicationError>
    where
        F: Future,
    {
        self.ensure_active()?;
        let Some(deadline) = self.deadline.as_ref() else {
            return Ok(future.await);
        };
        let remaining = duration_until(deadline, &timestamp_now())
            .ok_or_else(ApplicationError::deadline_exceeded)?;
        tokio::time::timeout(remaining, future)
            .await
            .map_err(|_| ApplicationError::deadline_exceeded())
    }

    pub(crate) fn response_context(&self, server_instance_id: &str) -> ResponseContext {
        ResponseContext {
            request_id: self.request_id.clone(),
            completed_at: Some(timestamp_now()),
            server_instance_id: server_instance_id.to_string(),
        }
    }
}

pub(crate) fn timestamp_now() -> Timestamp {
    system_time_to_timestamp(SystemTime::now())
}

pub(crate) fn system_time_to_timestamp(time: SystemTime) -> Timestamp {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => Timestamp {
            seconds: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            nanos: duration.subsec_nanos() as i32,
        },
        Err(error) => {
            let duration = error.duration();
            if duration.subsec_nanos() == 0 {
                Timestamp {
                    seconds: -(duration.as_secs() as i64),
                    nanos: 0,
                }
            } else {
                Timestamp {
                    seconds: -(duration.as_secs() as i64) - 1,
                    nanos: (1_000_000_000 - duration.subsec_nanos()) as i32,
                }
            }
        }
    }
}

pub(crate) fn timestamp_after(duration: Duration) -> Timestamp {
    system_time_to_timestamp(
        SystemTime::now()
            .checked_add(duration)
            .unwrap_or(SystemTime::now()),
    )
}

fn validate_timestamp(timestamp: &Timestamp, field: &str) -> Result<(), ApplicationError> {
    if !(MIN_PROTO_SECONDS..=MAX_PROTO_SECONDS).contains(&timestamp.seconds) {
        return Err(ApplicationError::invalid(
            field,
            "seconds are outside the Protobuf Timestamp range",
        ));
    }
    if !(0..1_000_000_000).contains(&timestamp.nanos) {
        return Err(ApplicationError::invalid(
            field,
            "nanos must be between 0 and 999999999",
        ));
    }
    Ok(())
}

fn timestamp_has_elapsed(deadline: &Timestamp, now: &Timestamp) -> bool {
    (deadline.seconds, deadline.nanos) <= (now.seconds, now.nanos)
}

fn duration_until(deadline: &Timestamp, now: &Timestamp) -> Option<Duration> {
    let nanos = i128::from(deadline.seconds - now.seconds)
        .checked_mul(1_000_000_000)?
        .checked_add(i128::from(deadline.nanos - now.nanos))?;
    if nanos <= 0 {
        return None;
    }
    let seconds = u64::try_from(nanos / 1_000_000_000).ok()?;
    let subsecond_nanos = u32::try_from(nanos % 1_000_000_000).ok()?;
    Some(Duration::new(seconds, subsecond_nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_elapsed_and_noncanonical_deadlines() {
        let elapsed = RequestContext {
            deadline: Some(Timestamp {
                seconds: 0,
                nanos: 0,
            }),
            ..RequestContext::default()
        };
        assert!(RequestScope::begin(Some(&elapsed)).is_err());

        let malformed = RequestContext {
            deadline: Some(Timestamp {
                seconds: MAX_PROTO_SECONDS,
                nanos: 1_000_000_000,
            }),
            ..RequestContext::default()
        };
        assert!(RequestScope::begin(Some(&malformed)).is_err());
    }

    #[test]
    fn response_context_uses_server_generated_correlation() {
        let context = RequestContext {
            client_request_id: Some("caller-value".to_string()),
            deadline: Some(timestamp_after(Duration::from_secs(60))),
            ..RequestContext::default()
        };
        let scope = RequestScope::begin(Some(&context)).unwrap();
        let response = scope.response_context("instance");
        assert_ne!(response.request_id, "caller-value");
        assert_eq!(response.server_instance_id, "instance");
        assert!(response.completed_at.is_some());
        assert!(scope.ensure_active().is_ok());
    }

    #[tokio::test]
    async fn wait_stops_at_the_request_deadline() {
        let context = RequestContext {
            deadline: Some(timestamp_after(Duration::from_millis(10))),
            ..RequestContext::default()
        };
        let scope = RequestScope::begin(Some(&context)).unwrap();
        let error = scope.wait(std::future::pending::<()>()).await.unwrap_err();
        assert_eq!(
            error.code(),
            ddb_api_types::v2::DdbErrorCode::DeadlineExceeded
        );
    }
}
