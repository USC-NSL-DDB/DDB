use ddb_api_types::v2::{DdbError, DdbErrorCode};

/// Result type returned by the public DDB client.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Typed client failure. Server-side semantic failures retain the stable
/// `DdbError` contract instead of flattening it into display text.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid DDB endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("invalid DDB client configuration: {0}")]
    InvalidConfig(String),
    #[error("DDB transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("DDB returned HTTP {status}: {message}")]
    Api {
        status: reqwest::StatusCode,
        message: String,
        error: Box<DdbError>,
    },
    #[error("DDB returned HTTP {status} without a v2 error envelope: {message}")]
    Http {
        status: reqwest::StatusCode,
        message: String,
    },
    #[error("invalid DDB wire response: {0}")]
    Protocol(String),
    #[error("DDB payload exceeded the configured {limit}-byte limit")]
    PayloadTooLarge { limit: usize },
    #[error("DDB collection exceeded the configured {limit}-item limit")]
    CollectionTooLarge { limit: usize },
    #[error("DDB event stream ended")]
    StreamEnded,
    #[error("DDB operation {operation_id} did not complete before the client deadline")]
    OperationTimeout { operation_id: String },
    #[error("DDB reconnect failed after {attempts} attempts: {last_error}")]
    ReconnectExhausted {
        attempts: u32,
        last_error: Box<ClientError>,
    },
}

impl ClientError {
    /// Returns the stable server error when this failure came from DDB.
    pub fn ddb_error(&self) -> Option<&DdbError> {
        match self {
            Self::Api { error, .. } => Some(error),
            _ => None,
        }
    }

    /// True when state must be rehydrated rather than retried from its cursor.
    pub fn requires_rehydration(&self) -> bool {
        self.ddb_error().is_some_and(|error| {
            error.code == DdbErrorCode::ReplayGap as i32
                || error.code == DdbErrorCode::Expired as i32
        })
    }

    /// True when retrying a read or a stream connection can be useful.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::StreamEnded => true,
            Self::Api { error, .. } => {
                error.retryable
                    || matches!(
                        DdbErrorCode::try_from(error.code),
                        Ok(DdbErrorCode::NotReady | DdbErrorCode::Unavailable)
                    )
            }
            _ => false,
        }
    }

    /// True only when the endpoint explicitly reports that API v2 is absent.
    /// Authentication, permission, connectivity, and malformed responses must
    /// never be treated as permission to downgrade to v1.
    pub fn is_api_version_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: reqwest::StatusCode::NOT_FOUND,
                ..
            }
        )
    }
}
