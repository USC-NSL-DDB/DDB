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
- Notification fanout is measured separately from command fanout.
  The debugger has a distinct WebSocket delivery plane and it should not be inferred from command latency.

## Recommended Next Steps

- Track the JSON output in CI and gate on percentile regressions instead of averages.
- Add a targeted benchmark for `-exec-continue` / stop-event latency once that path is made deterministic enough for repeated automated runs.
