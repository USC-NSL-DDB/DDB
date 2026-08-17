//! Low-cardinality API telemetry and safe cross-transport trace propagation.
//!
//! Metric attributes are deliberately limited to contract metadata. Raw
//! debugger commands, expressions, source paths/content, memory, credentials,
//! and extension payloads never enter this module.

use std::{sync::OnceLock, time::Instant};

use axum::{
    extract::{MatchedPath, Request},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use ddb_api_types::{
    v2::{Operation, OperationKind, OperationState, PermissionScope, StateEventKind},
    wkt::Timestamp,
};
use opentelemetry::{
    global,
    metrics::{Counter, Histogram, UpDownCounter},
    propagation::Extractor,
    KeyValue,
};
use tracing::{info, info_span, Instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use prost::Message;

#[cfg(feature = "grpc-preview")]
use tracing::Span;

#[cfg(feature = "grpc-preview")]
use tonic::metadata::MetadataMap;

struct ApiMetrics {
    requests: Counter<u64>,
    request_failures: Counter<u64>,
    request_duration_ms: Histogram<f64>,
    request_bytes: Histogram<u64>,
    response_bytes: Histogram<u64>,
    compatibility_requests: Counter<u64>,
    authorization_decisions: Counter<u64>,
    operation_transitions: Counter<u64>,
    operation_failures: Counter<u64>,
    operation_duration_ms: Histogram<f64>,
    operation_record_bytes: Histogram<u64>,
    operation_store_records: Histogram<u64>,
    operation_store_bytes: Histogram<u64>,
    idempotent_replays: Counter<u64>,
    state_events: Counter<u64>,
    state_event_bytes: Histogram<u64>,
    state_journal_events: Histogram<u64>,
    state_journal_bytes: Histogram<u64>,
    replay_gaps: Counter<u64>,
    active_subscribers: UpDownCounter<i64>,
    output_gaps: Counter<u64>,
    output_dropped_events: Counter<u64>,
    output_truncations: Counter<u64>,
}

fn metrics() -> &'static ApiMetrics {
    static METRICS: OnceLock<ApiMetrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = global::meter("ddb.api");
        ApiMetrics {
            requests: meter
                .u64_counter("ddb.api.server.requests")
                .with_description("Completed API requests")
                .build(),
            request_failures: meter
                .u64_counter("ddb.api.server.failures")
                .with_description("API responses with an error status")
                .build(),
            request_duration_ms: meter
                .f64_histogram("ddb.api.server.duration")
                .with_unit("ms")
                .with_description("Time to produce API response headers")
                .build(),
            request_bytes: meter
                .u64_histogram("ddb.api.server.request_size")
                .with_unit("By")
                .with_description("Declared API request body size")
                .build(),
            response_bytes: meter
                .u64_histogram("ddb.api.server.response_size")
                .with_unit("By")
                .with_description("Declared API response body size")
                .build(),
            compatibility_requests: meter
                .u64_counter("ddb.api.compatibility.requests")
                .with_description("Requests served by frozen compatibility surfaces")
                .build(),
            authorization_decisions: meter
                .u64_counter("ddb.api.authorization.decisions")
                .with_description("API authorization decisions")
                .build(),
            operation_transitions: meter
                .u64_counter("ddb.api.operations.transitions")
                .with_description("Public operation lifecycle transitions")
                .build(),
            operation_failures: meter
                .u64_counter("ddb.api.operations.failures")
                .with_description("Public operations entering a failed state")
                .build(),
            operation_duration_ms: meter
                .f64_histogram("ddb.api.operations.duration")
                .with_unit("ms")
                .with_description("Elapsed time from operation admission to terminal state")
                .build(),
            operation_record_bytes: meter
                .u64_histogram("ddb.api.operations.record_size")
                .with_unit("By")
                .with_description("Encoded size of bounded public operation records")
                .build(),
            operation_store_records: meter
                .u64_histogram("ddb.api.operations.retained_records")
                .with_description("Retained operation-record count sampled after store access")
                .build(),
            operation_store_bytes: meter
                .u64_histogram("ddb.api.operations.reserved_bytes")
                .with_unit("By")
                .with_description("Reserved operation-store bytes sampled after store access")
                .build(),
            idempotent_replays: meter
                .u64_counter("ddb.api.operations.idempotent_replays")
                .with_description("Mutations deduplicated by idempotency key")
                .build(),
            state_events: meter
                .u64_counter("ddb.api.state.events")
                .with_description("Committed replayable state events")
                .build(),
            state_event_bytes: meter
                .u64_histogram("ddb.api.state.event_size")
                .with_unit("By")
                .with_description("Encoded state-event size")
                .build(),
            state_journal_events: meter
                .u64_histogram("ddb.api.state.retained_events")
                .with_description("Retained state-event count sampled after journal access")
                .build(),
            state_journal_bytes: meter
                .u64_histogram("ddb.api.state.retained_bytes")
                .with_unit("By")
                .with_description("Retained state-journal bytes sampled after journal access")
                .build(),
            replay_gaps: meter
                .u64_counter("ddb.api.stream.replay_gaps")
                .with_description("Stream cursors requiring resynchronization")
                .build(),
            active_subscribers: meter
                .i64_up_down_counter("ddb.api.stream.active_subscribers")
                .with_description("Current API stream subscribers")
                .build(),
            output_gaps: meter
                .u64_counter("ddb.api.output.gaps")
                .with_description("Explicit output-loss records delivered")
                .build(),
            output_dropped_events: meter
                .u64_counter("ddb.api.output.dropped_events")
                .with_description("Output events represented by gap records")
                .build(),
            output_truncations: meter
                .u64_counter("ddb.api.output.truncations")
                .with_description("Output records truncated at the configured byte bound")
                .build(),
        }
    })
}

pub(crate) async fn observe_http(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_string();
    let route = route_name(&request).to_string();
    let class = route_class(request.uri().path());
    let request_bytes = content_length(request.headers());
    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let span = info_span!(
        "ddb.api.http",
        http.request.method = %method,
        http.route = %route,
        ddb.api.route_class = class,
    );
    let _ = span.set_parent(parent);
    let started = Instant::now();
    let response = next.run(request).instrument(span).await;
    let status = response.status().as_u16() as i64;
    let response_bytes = content_length(response.headers());
    let attributes = [
        KeyValue::new("http.request.method", method),
        KeyValue::new("http.route", route),
        KeyValue::new("http.response.status_code", status),
        KeyValue::new("ddb.api.route_class", class),
    ];
    metrics().requests.add(1, &attributes);
    if status >= 400 {
        metrics().request_failures.add(1, &attributes);
    }
    metrics()
        .request_duration_ms
        .record(started.elapsed().as_secs_f64() * 1_000.0, &attributes);
    if let Some(bytes) = request_bytes {
        metrics().request_bytes.record(bytes, &attributes);
    }
    if let Some(bytes) = response_bytes {
        metrics().response_bytes.record(bytes, &attributes);
    }
    if matches!(class, "v1" | "legacy") {
        metrics().compatibility_requests.add(1, &attributes);
    }
    response
}

#[cfg(feature = "grpc-preview")]
pub(crate) fn record_grpc_request(
    method: &'static str,
    started: Instant,
    request_bytes: usize,
    response_bytes: Option<usize>,
    status: tonic::Code,
) {
    let attributes = [
        KeyValue::new("network.protocol.name", "grpc"),
        KeyValue::new("rpc.method", method),
        KeyValue::new("rpc.grpc.status_code", status as i64),
        KeyValue::new("ddb.api.route_class", "v2"),
    ];
    metrics().requests.add(1, &attributes);
    if status != tonic::Code::Ok {
        metrics().request_failures.add(1, &attributes);
    }
    metrics()
        .request_duration_ms
        .record(started.elapsed().as_secs_f64() * 1_000.0, &attributes);
    metrics()
        .request_bytes
        .record(request_bytes as u64, &attributes);
    if let Some(response_bytes) = response_bytes {
        metrics()
            .response_bytes
            .record(response_bytes as u64, &attributes);
    }
}

pub(crate) fn route_name(request: &Request) -> &str {
    request
        .extensions()
        .get::<MatchedPath>()
        .map_or("<unmatched>", MatchedPath::as_str)
}

pub(crate) fn record_authorization(
    transport: &'static str,
    method: &str,
    required: PermissionScope,
    decision: &'static str,
    principal: Option<&str>,
) {
    let required = scope_name(required);
    metrics().authorization_decisions.add(
        1,
        &[
            KeyValue::new("network.protocol.name", transport),
            KeyValue::new("rpc.method", method.to_string()),
            KeyValue::new("ddb.api.required_scope", required),
            KeyValue::new("ddb.api.authorization.decision", decision),
        ],
    );
    if required != "read" {
        info!(
            target: "ddb::api::audit",
            transport,
            api_method = method,
            required_scope = required,
            decision,
            principal = principal.unwrap_or("unauthenticated"),
            "privileged API authorization decision"
        );
    }
}

pub(crate) fn record_operation_transition(operation: &Operation) {
    let kind = OperationKind::try_from(operation.kind).unwrap_or(OperationKind::Unspecified);
    let state = OperationState::try_from(operation.state).unwrap_or(OperationState::Unspecified);
    let attributes = [
        KeyValue::new("ddb.api.operation.kind", kind.as_str_name()),
        KeyValue::new("ddb.api.operation.state", state.as_str_name()),
    ];
    metrics().operation_transitions.add(1, &attributes);
    if state == OperationState::Failed {
        metrics().operation_failures.add(1, &attributes);
    }
    let record_bytes = operation.encoded_len() as u64;
    metrics()
        .operation_record_bytes
        .record(record_bytes, &attributes);
    let duration_ms = operation
        .accepted_at
        .as_ref()
        .zip(operation.completed_at.as_ref())
        .and_then(|(accepted, completed)| timestamp_duration_ms(accepted, completed));
    if let Some(duration_ms) = duration_ms {
        metrics()
            .operation_duration_ms
            .record(duration_ms, &attributes);
    }

    info!(
        target: "ddb::api::operation",
        operation_id = %operation.operation_id,
        request_id = %operation.request_id,
        operation_kind = kind.as_str_name(),
        operation_state = state.as_str_name(),
        target_count = operation.target.as_ref().map_or(0, |target| target.resolved_target_count),
        outcome_count = operation.target_outcomes.len(),
        has_error = operation.error.is_some(),
        record_bytes,
        duration_ms = ?duration_ms,
        "API operation lifecycle transition"
    );
}

pub(crate) fn record_idempotent_replay(kind: OperationKind) {
    metrics().idempotent_replays.add(
        1,
        &[KeyValue::new("ddb.api.operation.kind", kind.as_str_name())],
    );
}

pub(crate) fn record_operation_store_depth(records: usize, reserved_bytes: usize) {
    metrics()
        .operation_store_records
        .record(records as u64, &[]);
    metrics()
        .operation_store_bytes
        .record(reserved_bytes as u64, &[]);
}

pub(crate) fn record_state_event(kind: StateEventKind, encoded_bytes: usize) {
    let attributes = [KeyValue::new("ddb.api.event.kind", kind.as_str_name())];
    metrics().state_events.add(1, &attributes);
    metrics()
        .state_event_bytes
        .record(encoded_bytes as u64, &attributes);
}

pub(crate) fn record_state_journal_depth(events: usize, retained_bytes: usize) {
    metrics().state_journal_events.record(events as u64, &[]);
    metrics()
        .state_journal_bytes
        .record(retained_bytes as u64, &[]);
}

pub(crate) fn record_replay_gap(lane: &'static str) {
    metrics()
        .replay_gaps
        .add(1, &[KeyValue::new("ddb.api.stream.lane", lane)]);
}

pub(crate) fn record_subscriber_delta(lane: &'static str, delta: i64) {
    metrics()
        .active_subscribers
        .add(delta, &[KeyValue::new("ddb.api.stream.lane", lane)]);
}

pub(crate) fn record_output_gap(dropped_events: u64) {
    let attributes = [KeyValue::new("ddb.api.stream.lane", "output")];
    metrics().output_gaps.add(1, &attributes);
    metrics()
        .output_dropped_events
        .add(dropped_events, &attributes);
}

pub(crate) fn record_output_truncation() {
    metrics()
        .output_truncations
        .add(1, &[KeyValue::new("ddb.api.stream.lane", "output")]);
}

#[cfg(feature = "grpc-preview")]
pub(crate) fn attach_grpc_trace_parent<T>(request: &tonic::Request<T>) {
    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&MetadataExtractor(request.metadata()))
    });
    let _ = Span::current().set_parent(parent);
}

fn route_class(path: &str) -> &'static str {
    if path.starts_with("/api/v2/") {
        "v2"
    } else if path == "/api/v1" || path.starts_with("/api/v1/") {
        "v1"
    } else {
        "legacy"
    }
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn timestamp_duration_ms(start: &Timestamp, end: &Timestamp) -> Option<f64> {
    let seconds = end.seconds.checked_sub(start.seconds)?;
    let nanos = i64::from(end.nanos).checked_sub(i64::from(start.nanos))?;
    let total_nanos = seconds.checked_mul(1_000_000_000)?.checked_add(nanos)?;
    (total_nanos >= 0).then_some(total_nanos as f64 / 1_000_000.0)
}

fn scope_name(scope: PermissionScope) -> &'static str {
    match scope {
        PermissionScope::Read => "read",
        PermissionScope::Control => "control",
        PermissionScope::Admin => "admin",
        PermissionScope::Unspecified => "unspecified",
    }
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|key| key.as_str()).collect()
    }
}

#[cfg(feature = "grpc-preview")]
struct MetadataExtractor<'a>(&'a MetadataMap);

#[cfg(feature = "grpc-preview")]
impl Extractor for MetadataExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        vec!["traceparent", "tracestate"]
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedWriter(Arc::clone(&self.0))
        }
    }

    #[test]
    fn route_classes_are_bounded_and_do_not_include_parameters() {
        assert_eq!(route_class("/api/v2/rpc/service/Method"), "v2");
        assert_eq!(route_class("/api/v1/breakpoints/secret"), "v1");
        assert_eq!(route_class("/send?cmd=secret"), "legacy");
    }

    #[test]
    fn operation_duration_rejects_reversed_timestamps() {
        assert_eq!(
            timestamp_duration_ms(
                &Timestamp {
                    seconds: 10,
                    nanos: 500_000_000,
                },
                &Timestamp {
                    seconds: 11,
                    nanos: 250_000_000,
                },
            ),
            Some(750.0)
        );
        assert_eq!(
            timestamp_duration_ms(
                &Timestamp {
                    seconds: 11,
                    nanos: 0,
                },
                &Timestamp {
                    seconds: 10,
                    nanos: 0,
                },
            ),
            None
        );
    }

    #[test]
    fn operation_logs_exclude_error_and_debugger_payload_text() {
        const SECRET: &str = "sentinel-token-expression-command-memory-source";
        let operation = Operation {
            operation_id: "op_safe_reference".to_string(),
            request_id: "req_safe_reference".to_string(),
            kind: OperationKind::RawCommand as i32,
            state: OperationState::Failed as i32,
            error: Some(ddb_api_types::v2::DdbError {
                code: ddb_api_types::v2::DdbErrorCode::BackendFailed as i32,
                message: SECRET.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let captured = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            record_operation_transition(&operation);
        });

        let logs = String::from_utf8(
            captured
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        )
        .expect("captured tracing output should be UTF-8");
        assert!(logs.contains("op_safe_reference"));
        assert!(logs.contains("has_error=true"));
        assert!(!logs.contains(SECRET));
    }
}
