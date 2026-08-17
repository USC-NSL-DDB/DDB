# Benchmark Suite

The `ddb-bench` workspace tool answers debugger-scale questions such as:

- How command latency changes as the number of attached processes grows.
- Whether the hot path is command admission, session-runtime fanout, response projection, or notification delivery.
- How much cold-start time is spent bringing mock sessions online before any user command runs.

The harness follows the production process boundary rather than relying on generic microbenchmarks.

## Why These Scenarios

The codebase has a few distinct execution paths that matter for debugger responsiveness:

- Startup path:
  `DbgManager::init_static_sessions` -> `SessionSupervisor::admit` -> `SessionActivation::activate`
- CLI command path:
  `CommandEngine::execute_cli` -> `CommandDispatcher` -> command service -> `Router` -> session runtime -> stdout
- API command path:
  `POST /send` -> `CommandEngine::execute_api` -> command service -> `Router` -> session runtime -> JSON response
- Notification path:
  `NotificationManager::broadcast` -> per-subscriber queue send

Those paths live in:

- `core/src/dbg_mgr.rs`
- `core/src/session/activation.rs`
- `core/src/cmd_flow/input.rs`
- `core/src/cmd_flow/engine.rs`
- `core/src/cmd_flow/dispatcher.rs`
- `core/src/cmd_flow/router.rs`
- `core/src/cmd_flow/session_runtime/`
- `core/src/api/server.rs`
- `core/src/notification/manager.rs`

## Benchmark Matrix

The suite now has two tiers:

- Mock-backed scale benchmarks for command fanout and notification behavior.
- A real GDB-backed distributed-backtrace benchmark that uses a synthetic multi-process fixture.

Current scenarios:

- `startup`
  Measures cold-start time until all static mock sessions are attached and visible from `/sessions`.
- `api-thread-info`
  Measures API round-trip latency for broadcast `-thread-info`.
- `api-thread-info-burst`
  Measures concurrent API broadcasts to expose admission, pipelining, correlation, and fanout contention.
- `api-list-groups`
  Measures API round-trip latency for broadcast `-list-thread-groups`.
- `v2-http-snapshot`
  Measures a validated full seven-section API v2 snapshot over a reused
  HTTP/ProtoJSON connection.
- `v2-grpc-snapshot`
  Measures the identical application-service snapshot over a reused Tonic
  gRPC/Protobuf connection. This requires a DDB binary built with
  `grpc-preview`.
- `v2-http-step-stop`
  Measures typed `NEXT` admission through HTTP/ProtoJSON until the same
  thread publishes a replayable stopped event at a later source line.
- `v2-grpc-step-stop`
  Measures the identical typed `NEXT`-to-stopped workflow over reused
  Tonic/Protobuf unary and state-stream connections. This also requires
  `grpc-preview`.
- `v2-http-drained-output-step`
  Measures HTTP `NEXT`-to-stopped latency while the same bounded output load as
  the slow-consumer scenario is actively drained. This is the comparison
  baseline that separates output-processing cost from consumer backpressure.
- `v2-http-mixed-output-step`
  Measures HTTP `NEXT`-to-stopped latency while a separate output subscription
  is deliberately left unread and flooded through the independently bounded
  output lane. This scenario is opt-in because it generates bulk output.
- `v2-http-variable-inspection`
  Measures large variable inspection through the public Rust SDK. The fixture
  exposes at most 500 deterministic roots per frame and the client retrieves
  the requested total in bounded, validated pages (10,000 variables by
  default).
- `v2-http-memory-transfer`
  Measures 1/16/64 MiB logical memory reads through repeated public
  `ReadMemory` calls. Every request remains bounded to at most 1 MiB and every
  returned chunk is validated before it is counted.
- `v2-http-state-fanout`
  Measures one typed `NEXT` command until 1/8/20 concurrent public state
  subscribers all observe the same later stopped location.
- `v2-http-reconnect-replay`
  Measures SDK projection recovery after a forced transport reconnect. It
  validates the initial snapshot, resumes from its replay cursor, executes
  `NEXT`, and waits for the reconstructed projection to observe that stop.
- `cli-thread-info`
  Measures full CLI command-engine latency for `-thread-info`.
- `cli-break-insert`
  Measures group-targeted breakpoint insertion through the breakpoint service.
- `notifications`
  Measures WebSocket notification fanout latency to subscribed clients.
- `distributed-backtrace`
  Measures end-to-end `-bt-remote` latency using a real dummy application chain, including remote metadata extraction, parent interrupt, context switch, and recursive stack aggregation.
- `lldb-distributed-backtrace`
  Runs the identical real distributed-backtrace chain through LLDB, making
  backend cost and regressions directly comparable.
  The command timer starts only after every session is stopped and its register
  context has been provisioned. Consequently, LLDB's default one-time stack
  warmup is charged to session readiness rather than this command-latency
  metric. Pass `--lldb-eager-stack-warmup false` to measure cold LLDB command
  behavior and keep the selected policy alongside results in JSON output.
  The harness does not issue a remote-metadata lookup before the timed
  `-bt-remote`; the timer includes command submission, and either a correlated
  success or error result terminates the wait so failed samples fail promptly.

Primary scaling axis:

- attached sessions: default `1,4,16,64`

Secondary scaling axes:

- threads per session
- notification subscribers
- total inspected variables and deterministic variables per Mock frame
- total logical memory bytes and bounded bytes per `ReadMemory` call
- concurrent public state subscribers
- distributed backtrace depth (`1..=16`)

## Running

Example:

```bash
cargo run -p ddb-bench --release -- \
  --scales 1,4,16,64 \
  --threads-per-session 4 \
  --notification-subscribers 8
```

JSON output for regression tooling:

```bash
cargo run -p ddb-bench --release -- --format json
```

Write credential-free JSON evidence directly to a file with `--output`; this is
accepted only with `--format json`:

```bash
cargo run -p ddb-bench --release -- \
  --format json \
  --output benchmarks/evidence/local-run.json
```

The transport comparison and its limitations are retained under
[`benchmarks/evidence/2026-08-14-v2-transport`](../benchmarks/evidence/2026-08-14-v2-transport/README.md).
The three-run typed control and slow-output comparison is retained under
[`benchmarks/evidence/2026-08-14-v2-control-output`](../benchmarks/evidence/2026-08-14-v2-control-output/README.md).
The three-run large-inspection, memory, state-fanout, and reconnect/replay
matrix is retained under
[`benchmarks/evidence/2026-08-14-v2-inspection-replay`](../benchmarks/evidence/2026-08-14-v2-inspection-replay/README.md).

Large inspection, memory, state-fanout, and reconnect/replay matrix:

```bash
cargo build -p ddb --release --features grpc-preview
cargo run -p ddb-bench --release -- \
  --binary target/release/ddb \
  --scenarios v2-http-variable-inspection,v2-http-memory-transfer,v2-http-state-fanout,v2-http-reconnect-replay \
  --inspection-variables 10000 \
  --variables-per-frame 500 \
  --memory-sizes-mib 1,16,64 \
  --memory-chunk-bytes 1048576 \
  --state-subscribers 1,8,20 \
  --scales 1,16,64 \
  --threads-per-session 1 \
  --format json
```

These workloads are opt-in so the default local benchmark remains quick.
`--inspection-variables` is bounded to `1..=1000000`,
`--variables-per-frame` to `1..=500`, `--memory-sizes-mib` to `1..=1024`,
`--memory-chunk-bytes` to `1..=1048576`, and `--state-subscribers` to `1..=20`.
Reconnect/replay uses `--scales` as its session axis.

Typed control-to-stop and mixed-output smoke matrix:

```bash
cargo build -p ddb --release --features grpc-preview
cargo run -p ddb-bench -- \
  --binary target/release/ddb \
  --scenarios v2-http-step-stop,v2-grpc-step-stop,v2-http-drained-output-step,v2-http-mixed-output-step \
  --scales 1 \
  --threads-per-session 1 \
  --bulk-output-events 2048 \
  --bulk-output-event-bytes 4096 \
  --format json
```

`--bulk-output-events` is bounded to `1..=4096` and
`--bulk-output-event-bytes` to `1..=65536`. The JSON report records both
inputs and their total generated bytes so evidence is reproducible.

Distributed backtrace depth sweep:

```bash
cargo run -p ddb-bench --release -- \
  --scenarios distributed-backtrace,lldb-distributed-backtrace \
  --dbt-depths 1,2,4,8,16
```

The tool rebuilds `target/release/ddb` automatically before each run so the benchmarked binary matches the current source tree.
If you want to reuse an already-built binary instead, pass `--binary /path/to/ddb`.

## Design Choices

- The harness spawns the real `ddb` binary instead of calling internals directly.
  This keeps the benchmark honest about thread layout, API overhead, stdout emission, and notification behavior.
- Mock sessions are generated dynamically for each scale point.
  That makes session-count sweeps deterministic and cheap enough for local iteration.
- The distributed-backtrace benchmark uses a real dummy application instead of the mock backend.
  The runtime GDB script needs real frames, local variables, interrupts, and register context switching to make the measurement meaningful.
- API and CLI scenarios are both present.
  Both use the same command engine and dispatcher while retaining their distinct targeting and presentation policies.
- The v2 HTTP and gRPC scenarios validate equivalent snapshot content on every
  sample and use the same transport-independent application service. Their
  comparison does not imply conclusions about workloads or resource metrics
  that were not measured.
- The typed step scenarios start timing immediately before `Execute(NEXT)` and
  stop only after the selected thread publishes a stopped state at a strictly
  later source line. Operation causation is matched when backend events carry
  it; tokenless debugger stop notifications are correlated by the exact thread
  and monotonic line instead of being misrepresented as command completion.
- The mixed-output scenario holds an output response body without polling it.
  A bounded Mock producer is started through the public GDB-MI compatibility
  facade, acknowledges immediately, and emits on a separate task; it therefore
  does not occupy the session command lane ahead of the timed typed v2 control.
  Compare it with `v2-http-drained-output-step`, not the no-output scenario, to
  isolate the incremental effect of a slow consumer from the real cost of
  parsing and routing the configured output volume.
- Variable inspection uses the SDK's recursive variable collection API and
  bounded page size instead of adding a benchmark-only bulk endpoint. Totals
  larger than one frame are measured as repeated real collections; a final
  non-divisible prefix uses an explicit bounded page loop. This keeps the
  workload exact and deterministic while exercising the public paging path.
- Memory transfer is a logical workload, not an invitation to increase the API
  response limit: large totals are composed from advancing, bounded 1
  MiB-or-smaller chunks and the harness rejects unreadable or short results.
- State fanout and reconnect/replay consume the public SDK event/projection
  APIs. They therefore include stream establishment, cursor semantics, event
  decoding, and projection convergence rather than benchmarking an internal
  channel.
- Notification fanout is measured separately from command fanout.
  The debugger has a distinct WebSocket delivery plane and it should not be inferred from command latency.

## Recommended Next Steps

- Track the JSON output in CI and gate on percentile regressions instead of averages.
- Expand the retained latency evidence with CPU, RSS, allocation, throughput,
  and complete wire-byte collection before making broader resource-efficiency
  or transport performance claims.
