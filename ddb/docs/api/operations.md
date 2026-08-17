# Operating the DDB API

## Health, readiness, and shutdown

`GetHealth` reports process/component liveness and is intentionally usable
without credentials. `GetReadiness` becomes ready only after all runtime
components are up. Both responses contain minimal component state and the
server-instance context; they do not contain debugger payloads.

The HTTP listener stops accepting new connections when DDB shutdown begins,
closes application-owned state/output subscriptions, then waits for active
responses to drain. This ordering ensures a long-lived event stream cannot
deadlock graceful shutdown. `Shutdown` requires `ADMIN`, a broadcast target,
and an idempotency key.

## Partial fanout operations

A partially successful fanout mutation terminates in
`OPERATION_STATE_FAILED`, uses `DDB_ERROR_CODE_PARTIAL_FAILURE`, and carries an
outcome for every target. When the mutation creates or changes a resource, its
typed result is the authoritative state that remains after the partial
failure. Clients should apply that result before presenting retry or cleanup
controls.

## OpenTelemetry

Start DDB with `--enable-otel --otel-endpoint <collector-grpc-uri>` to export
traces, logs, and metrics through the existing OTLP/gRPC pipeline. HTTP and the
optional gRPC preview extract standard W3C `traceparent`/`tracestate` context.
Untrusted trace context affects correlation only and never authorization.

API metrics use low-cardinality contract attributes such as static route,
transport, status, scope, lane, operation kind/state, and event kind. They
never use resource IDs, principal IDs, source paths, commands, or payloads as
metric labels.

| Metric | Meaning |
|---|---|
| `ddb.api.server.requests` / `ddb.api.server.failures` | Completed HTTP or gRPC request and error count by static route/method and status. |
| `ddb.api.server.duration` | Milliseconds to HTTP response headers or gRPC unary/stream admission; stream lifetime is separate. |
| `ddb.api.server.request_size` / `ddb.api.server.response_size` | Declared HTTP body bytes or encoded gRPC message bytes; payload content is never recorded. |
| `ddb.api.compatibility.requests` | v1 and unversioned route usage without payload telemetry. |
| `ddb.api.authorization.decisions` | Allowed/denied decisions by transport, method, and required scope. |
| `ddb.api.operations.transitions` / `ddb.api.operations.failures` | Accepted/running/terminal transitions and failures by operation kind/state. |
| `ddb.api.operations.duration` / `ddb.api.operations.record_size` | Admission-to-terminal duration and bounded encoded record size. |
| `ddb.api.operations.retained_records` / `ddb.api.operations.reserved_bytes` | Operation-store depth sampled after access/pruning. |
| `ddb.api.operations.idempotent_replays` | Retried mutations returned from the deduplication record. |
| `ddb.api.state.events` / `ddb.api.state.event_size` | Replayable event volume and encoded bytes. |
| `ddb.api.state.retained_events` / `ddb.api.state.retained_bytes` | State-journal depth sampled after access/pruning. |
| `ddb.api.stream.replay_gaps` | State/output cursors that require resync. |
| `ddb.api.stream.active_subscribers` | Current state/output subscribers by lane. |
| `ddb.api.output.gaps` / `ddb.api.output.dropped_events` | Explicit output loss delivered to clients. |
| `ddb.api.output.truncations` | Records truncated at the configured output byte limit. |

Alert on repeated authorization denials, operation failures, replay gaps,
subscriber saturation, output gaps, and readiness loss. A compatibility-route
counter supports migration planning but does not authorize removing v1 before
the published support window.

## Capacity and client diagnostics

Clients read effective page, payload, replay, subscriber, operation, memory,
source, variable, and extension bounds from `GetCapabilities`; operators should
not infer them from defaults in this document. `RESOURCE_EXHAUSTED` identifies
the violated class without echoing sensitive content. `REPLAY_GAP` means the
client must discard its projection and hydrate a new snapshot. `OutputGap`
means presentation output was lost; it does not invalidate state convergence.
Each output subscription has a non-blocking pump from shared ingress into its
own bounded queue. When that queue fills, the pump keeps draining ingress and
collapses lost records into an `OutputGap`; known subscriber-queue loss reports
both event and UTF-8 byte counts. Loss that occurred before subscription or in
the shared ingress queue has an unknown byte count rather than a fabricated
value.
The advertised `max_response_bytes` is enforced after ProtoJSON encoding for
every unary v2 response. `max_source_bytes` is the whole-file eligibility bound;
`max_source_lines` independently bounds each returned source window.

The API-owned replay, stream, output, and operation bounds are configurable
under `Conf.ApiLimits`. Defaults are intentionally conservative and match the
values advertised by `GetCapabilities`:

```yaml
Conf:
  ApiLimits:
    state_replay_events: 10000
    state_replay_bytes: 33554432
    state_replay_retention_millis: 300000
    state_subscriber_queue: 1024
    output_subscriber_queue: 2048
    max_subscribers: 20
    operation_records: 1024
    operation_bytes: 67108864
    operation_record_bytes: 65536
    operation_retention_millis: 900000
    output_event_bytes: 262144
```

Each configured value must be non-zero. Startup also rejects replay/operation
stores above 1 GiB, queues above 65,536 entries, more than 1,024 subscribers
per lane, more than 100,000 operation records, individual retained records
above 16 MiB, or retention longer than 24 hours. `operation_record_bytes`
cannot exceed `operation_bytes`. These ceilings prevent accidental startup
with an unbounded memory commitment; raise them only through a reviewed code
change backed by load evidence.

Use the public conformance runner as a deployment smoke test:

```bash
cargo run -p ddb-api-conformance -- \
  --endpoint http://127.0.0.1:5000 \
  --token "$DDB_API_TOKEN" --output json
```

Use the `mock` profile only against a disposable Mock DDB because it admits
control operations.
