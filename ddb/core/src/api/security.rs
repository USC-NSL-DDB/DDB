//! Deployment and HTTP admission policy for the public API listener.
//!
//! This module deliberately does not infer trust from forwarding headers.
//! Remote exposure is an operator decision made at startup, while bearer
//! authentication remains an independent request-level decision.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{bail, Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use ddb_api_types::v2::DdbErrorCode;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower_http::cors::{AllowOrigin, CorsLayer};

use super::application::ApplicationError;
use crate::common::Config;

#[cfg(test)]
pub(crate) fn validate_api_deployment(config: &Config) -> Result<()> {
    validate_api_deployment_with_options(config, false)
}

pub(crate) fn validate_api_deployment_with_options(
    config: &Config,
    allow_ephemeral_port: bool,
) -> Result<()> {
    let conf = &config.conf;
    if conf.api_server_port == 0 && !allow_ephemeral_port {
        bail!("Conf.api_server_port must be a non-zero port");
    }
    if conf.api_max_concurrent_requests == 0 {
        bail!("Conf.api_max_concurrent_requests must be non-zero");
    }
    if conf.api_requests_per_second == 0 {
        bail!("Conf.api_requests_per_second must be non-zero");
    }
    validate_resource_limits(&conf.api_limits)?;
    parse_allowed_origins(&conf.api_cors_allowed_origins)?;

    if conf.api_server_bind.is_loopback() {
        return Ok(());
    }
    if conf.api_insecure_allow_remote {
        return Ok(());
    }
    if conf.api_auth_token_file.is_none() {
        bail!(
            "remote Conf.api_server_bind={} requires Conf.api_auth_token_file",
            conf.api_server_bind
        );
    }
    if !conf.api_tls_terminated_by_trusted_proxy {
        bail!(
            "remote Conf.api_server_bind={} requires TLS through a trusted reverse proxy; set Conf.api_tls_terminated_by_trusted_proxy only when that boundary is enforced, or use Conf.api_insecure_allow_remote for development only",
            conf.api_server_bind
        );
    }
    Ok(())
}

fn validate_resource_limits(limits: &crate::common::config::ApiResourceLimits) -> Result<()> {
    const MAX_REPLAY_EVENTS: usize = 1_000_000;
    const MAX_RETAINED_BYTES: usize = 1024 * 1024 * 1024;
    const MAX_QUEUE: usize = 65_536;
    const MAX_SUBSCRIBERS: usize = 1_024;
    const MAX_OPERATION_RECORDS: usize = 100_000;
    const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
    const MAX_RETENTION_MILLIS: u64 = 24 * 60 * 60 * 1_000;

    validate_nonzero_bounded(
        "state_replay_events",
        limits.state_replay_events,
        MAX_REPLAY_EVENTS,
    )?;
    validate_nonzero_bounded(
        "state_replay_bytes",
        limits.state_replay_bytes,
        MAX_RETAINED_BYTES,
    )?;
    validate_nonzero_bounded(
        "state_subscriber_queue",
        limits.state_subscriber_queue,
        MAX_QUEUE,
    )?;
    validate_nonzero_bounded(
        "output_subscriber_queue",
        limits.output_subscriber_queue,
        MAX_QUEUE,
    )?;
    validate_nonzero_bounded("max_subscribers", limits.max_subscribers, MAX_SUBSCRIBERS)?;
    validate_nonzero_bounded(
        "operation_records",
        limits.operation_records,
        MAX_OPERATION_RECORDS,
    )?;
    validate_nonzero_bounded(
        "operation_bytes",
        limits.operation_bytes,
        MAX_RETAINED_BYTES,
    )?;
    validate_nonzero_bounded(
        "operation_record_bytes",
        limits.operation_record_bytes,
        MAX_RECORD_BYTES,
    )?;
    validate_nonzero_bounded(
        "output_event_bytes",
        limits.output_event_bytes,
        MAX_RECORD_BYTES,
    )?;
    if limits.operation_record_bytes > limits.operation_bytes {
        bail!("Conf.ApiLimits.operation_record_bytes must not exceed operation_bytes");
    }
    validate_retention(
        "state_replay_retention_millis",
        limits.state_replay_retention_millis,
        MAX_RETENTION_MILLIS,
    )?;
    validate_retention(
        "operation_retention_millis",
        limits.operation_retention_millis,
        MAX_RETENTION_MILLIS,
    )?;
    Ok(())
}

fn validate_nonzero_bounded(name: &str, value: usize, maximum: usize) -> Result<()> {
    if value == 0 || value > maximum {
        bail!("Conf.ApiLimits.{name} must be between 1 and {maximum}");
    }
    Ok(())
}

fn validate_retention(name: &str, value: u64, maximum: u64) -> Result<()> {
    if value == 0 || value > maximum {
        bail!("Conf.ApiLimits.{name} must be between 1 and {maximum}");
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct HttpAdmissionPolicy {
    allowed_origins: Arc<HashSet<HeaderValue>>,
    permits: Arc<Semaphore>,
    rate: Arc<Mutex<TokenBucket>>,
}

impl HttpAdmissionPolicy {
    pub(crate) fn from_config(config: &Config) -> Result<Self> {
        let origins = parse_allowed_origins(&config.conf.api_cors_allowed_origins)?;
        Ok(Self {
            allowed_origins: Arc::new(origins.into_iter().collect()),
            permits: Arc::new(Semaphore::new(config.conf.api_max_concurrent_requests)),
            rate: Arc::new(Mutex::new(TokenBucket::new(
                config.conf.api_requests_per_second,
            ))),
        })
    }

    pub(crate) fn cors_layer(&self) -> Option<CorsLayer> {
        if self.allowed_origins.is_empty() {
            return None;
        }
        Some(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(self.allowed_origins.iter().cloned()))
                .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        )
    }

    fn validate_headers(&self, headers: &HeaderMap) -> Result<(), PolicyRejection> {
        if let Some(encoding) = headers.get(header::CONTENT_ENCODING) {
            let identity = encoding
                .to_str()
                .map(|value| value.trim().eq_ignore_ascii_case("identity"))
                .unwrap_or(false);
            if !identity {
                return Err(PolicyRejection::UnsupportedContentEncoding);
            }
        }
        if let Some(origin) = headers.get(header::ORIGIN) {
            if !self.allowed_origins.contains(origin) {
                return Err(PolicyRejection::OriginDenied);
            }
        }
        Ok(())
    }

    fn try_rate(&self) -> bool {
        self.rate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_take()
    }

    fn try_acquire(&self) -> Result<OwnedSemaphorePermit, PolicyRejection> {
        Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| PolicyRejection::ConcurrencyExhausted)
    }
}

struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_second: f64,
    updated_at: Instant,
}

impl TokenBucket {
    fn new(requests_per_second: u32) -> Self {
        let capacity = f64::from(requests_per_second);
        Self {
            capacity,
            tokens: capacity,
            refill_per_second: capacity,
            updated_at: Instant::now(),
        }
    }

    fn try_take(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.updated_at).as_secs_f64();
        self.updated_at = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_second).min(self.capacity);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

#[derive(Clone, Copy, Debug)]
enum PolicyRejection {
    OriginDenied,
    UnsupportedContentEncoding,
    RateExceeded,
    ConcurrencyExhausted,
}

pub(crate) async fn enforce_http_policy(
    State(policy): State<Arc<HttpAdmissionPolicy>>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(rejection) = policy.validate_headers(request.headers()) {
        return rejection.into_response();
    }
    if !policy.try_rate() {
        return PolicyRejection::RateExceeded.into_response();
    }
    let permit = match policy.try_acquire() {
        Ok(permit) => permit,
        Err(rejection) => return rejection.into_response(),
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

impl IntoResponse for PolicyRejection {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::OriginDenied => (
                StatusCode::FORBIDDEN,
                ApplicationError::new(
                    DdbErrorCode::PermissionDenied,
                    "browser origin is not allowed by API policy",
                ),
            ),
            Self::UnsupportedContentEncoding => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ApplicationError::new(
                    DdbErrorCode::Unsupported,
                    "compressed request bodies are not supported",
                ),
            ),
            Self::RateExceeded => (
                StatusCode::TOO_MANY_REQUESTS,
                ApplicationError::resource_exhausted("API request rate limit exceeded"),
            ),
            Self::ConcurrencyExhausted => (
                StatusCode::SERVICE_UNAVAILABLE,
                ApplicationError::resource_exhausted("maximum concurrent API requests reached"),
            ),
        };
        let mut response = (
            status,
            Json(error.to_contract(uuid::Uuid::new_v4().to_string())),
        )
            .into_response();
        if matches!(self, Self::RateExceeded | Self::ConcurrencyExhausted) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

fn parse_allowed_origins(origins: &[String]) -> Result<Vec<HeaderValue>> {
    let mut parsed = Vec::with_capacity(origins.len());
    let mut seen = HashSet::with_capacity(origins.len());
    for origin in origins {
        if origin.trim() != origin || origin == "*" {
            bail!("CORS origins must be exact and must not contain whitespace or wildcards");
        }
        let uri: Uri = origin
            .parse()
            .with_context(|| format!("invalid CORS origin {origin:?}"))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
            bail!("CORS origin {origin:?} must use http or https and include an authority");
        }
        if uri
            .path_and_query()
            .is_some_and(|path| path.as_str() != "/")
        {
            bail!("CORS origin {origin:?} must not contain a path, query, or fragment");
        }
        let header = HeaderValue::from_str(origin)
            .with_context(|| format!("invalid CORS origin header {origin:?}"))?;
        if !seen.insert(header.clone()) {
            bail!("duplicate CORS origin {origin:?}");
        }
        parsed.push(header);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_is_loopback_only_by_default() {
        let config = Config::default();
        assert!(config.conf.api_server_bind.is_loopback());
        validate_api_deployment(&config).unwrap();
    }

    #[test]
    fn invalid_api_resource_limits_fail_at_startup() {
        let mut config = Config::default();
        config.conf.api_limits.max_subscribers = 0;
        assert!(validate_api_deployment(&config)
            .unwrap_err()
            .to_string()
            .contains("ApiLimits.max_subscribers"));

        let mut config = Config::default();
        config.conf.api_limits.state_subscriber_queue = 65_537;
        assert!(validate_api_deployment(&config)
            .unwrap_err()
            .to_string()
            .contains("ApiLimits.state_subscriber_queue"));

        let mut config = Config::default();
        config.conf.api_limits.operation_bytes = 1_024;
        config.conf.api_limits.operation_record_bytes = 2_048;
        assert!(validate_api_deployment(&config)
            .unwrap_err()
            .to_string()
            .contains("must not exceed operation_bytes"));
    }

    #[test]
    fn remote_bind_requires_authentication_and_transport_security() {
        let mut config = Config::default();
        config.conf.api_server_bind = "0.0.0.0".parse().unwrap();
        assert!(validate_api_deployment(&config)
            .unwrap_err()
            .to_string()
            .contains("api_auth_token_file"));

        config.conf.api_auth_token_file = Some("tokens.json".to_string());
        assert!(validate_api_deployment(&config)
            .unwrap_err()
            .to_string()
            .contains("trusted reverse proxy"));

        config.conf.api_tls_terminated_by_trusted_proxy = true;
        validate_api_deployment(&config).unwrap();
    }

    #[test]
    fn insecure_remote_override_is_explicit_and_does_not_enable_auth() {
        let mut config = Config::default();
        config.conf.api_server_bind = "192.0.2.10".parse().unwrap();
        config.conf.api_insecure_allow_remote = true;
        validate_api_deployment(&config).unwrap();
        assert!(!config.conf.api_insecure_allow_unauthenticated_v2);
    }

    #[test]
    fn cors_allowlist_accepts_origins_only() {
        assert_eq!(
            parse_allowed_origins(&["https://debug.example:8443".to_string()])
                .unwrap()
                .len(),
            1
        );
        for invalid in [
            "*",
            "debug.example",
            "https://debug.example/path",
            "https://debug.example?query=yes",
            " https://debug.example",
        ] {
            assert!(
                parse_allowed_origins(&[invalid.to_string()]).is_err(),
                "{invalid}"
            );
        }
        assert!(parse_allowed_origins(&[
            "https://debug.example".to_string(),
            "https://debug.example".to_string()
        ])
        .is_err());
    }

    #[test]
    fn admission_policy_rejects_unknown_origins_and_encodings() {
        let mut config = Config::default();
        config.conf.api_cors_allowed_origins = vec!["https://debug.example".to_string()];
        let policy = HttpAdmissionPolicy::from_config(&config).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(matches!(
            policy.validate_headers(&headers),
            Err(PolicyRejection::OriginDenied)
        ));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://debug.example"),
        );
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        assert!(matches!(
            policy.validate_headers(&headers),
            Err(PolicyRejection::UnsupportedContentEncoding)
        ));
    }

    #[test]
    fn admission_capacity_and_rate_are_hard_limits() {
        let mut config = Config::default();
        config.conf.api_max_concurrent_requests = 1;
        config.conf.api_requests_per_second = 1;
        let policy = HttpAdmissionPolicy::from_config(&config).unwrap();

        let permit = policy.try_acquire().unwrap();
        assert!(matches!(
            policy.try_acquire(),
            Err(PolicyRejection::ConcurrencyExhausted)
        ));
        drop(permit);
        assert!(policy.try_acquire().is_ok());

        assert!(policy.try_rate());
        assert!(!policy.try_rate());
    }
}
