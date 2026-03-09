# Benchmark Suite

This repository did not have a repeatable benchmark harness for debugger-scale questions such as:

- How command latency changes as the number of attached processes grows.
- Whether the hot path is the CLI handler layer, the API layer, the router/tracker fanout path, or notification delivery.
- How much cold-start time is spent bringing mock sessions online before any user command runs.

The new `ddb-bench` workspace tool is designed around the current code structure rather than generic microbenchmarks.

## Why These Scenarios

The codebase has a few distinct execution paths that matter for debugger responsiveness:

- Startup path:
  `DbgManager::init_static_sessions` -> `ServiceDiscover::start_session` -> `DbgSession::start`
- CLI command path:
  `CmdHandler::handle_cmd` -> command-specific handler -> router -> tracker -> stdout
- API command path:
  `POST /send` -> command-flow facade -> router -> tracker -> JSON response
- Notification path:
  `NotificationManager::broadcast` -> per-subscriber queue send

Those paths live in:

- `core/src/dbg_mgr.rs`
- `core/src/session/dbg_session.rs`
- `core/src/cmd_flow/input.rs`
- `core/src/cmd_flow/handler.rs`
- `core/src/cmd_flow/router.rs`
- `core/src/cmd_flow/tracker.rs`
- `core/src/api/server.rs`
- `core/src/notification/manager.rs`

## Benchmark Matrix

The suite intentionally uses the existing `mock` backend so it can scale session count without requiring real processes or remote hosts.

Current scenarios:

- `startup`
  Measures cold-start time until all static mock sessions are attached and visible from `/sessions`.
- `api-thread-info`
  Measures API round-trip latency for broadcast `-thread-info`.
- `api-list-groups`
  Measures API round-trip latency for broadcast `-list-thread-groups`.
- `cli-thread-info`
  Measures full CLI command-handler latency for `-thread-info`.
- `cli-break-insert`
  Measures group-targeted breakpoint insertion through the CLI handler layer.
- `notifications`
  Measures WebSocket notification fanout latency to subscribed clients.

Primary scaling axis:

- attached sessions: default `1,4,16,64`

Secondary scaling axes:

- threads per session
- notification subscribers

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

The tool rebuilds `target/release/ddb` automatically before each run so the benchmarked binary matches the current source tree.
If you want to reuse an already-built binary instead, pass `--binary /path/to/ddb`.

## Design Choices

- The harness spawns the real `ddb` binary instead of calling internals directly.
  This keeps the benchmark honest about thread layout, API overhead, stdout emission, and notification behavior.
- Mock sessions are generated dynamically for each scale point.
  That makes session-count sweeps deterministic and cheap enough for local iteration.
- API and CLI scenarios are both present.
  The API path bypasses command-specific CLI handlers; the CLI path includes them.
- Notification fanout is measured separately from command fanout.
  The debugger has a distinct WebSocket delivery plane and it should not be inferred from command latency.

## Recommended Next Steps

- Add a periodic `real-gdb` benchmark tier for a smaller scale set such as `1,4`.
- Track the JSON output in CI and gate on percentile regressions instead of averages.
- Add a targeted benchmark for `-exec-continue` / stop-event latency once that path is made deterministic enough for repeated automated runs.
