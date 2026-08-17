# DDB API v1

DDB's API is a first-class ingress over the same CommandEngine and
RuntimeModel used by the stdin client. It is not a second debugger
implementation: routing, distributed breakpoint semantics, global thread IDs,
execution ordering, and backend-neutral payload projection are shared.

The server listens on localhost:<Conf.api_server_port> and exposes:

- versioned JSON resources and operations under /api/v1
- structured debugger events at /api/v1/events (WebSocket)
- the original unversioned routes for compatibility

The default port is controlled by the existing configuration:

    Conf:
      api_server_port: 5000

## Response contract

Successful v1 responses use one envelope:

    {
      "api_version": "v1",
      "request_id": "221e40e8-07f2-4ad7-9ad8-f03d9dd45a85",
      "data": {}
    }

Failures use an HTTP status plus a stable machine code:

    {
      "api_version": "v1",
      "request_id": "4beec84f-8b26-47f0-a068-baaeb809db12",
      "error": {
        "code": "command_failed",
        "message": "No active sessions available for broadcast target",
        "details": {"external_token": null}
      }
    }

Debugger scalars are ordinary JSON strings, lists are JSON arrays, and records
are JSON objects. The tagged internal {"String": ...} / {"List": ...}
representation is never exposed by v1.

## Target contract

Operations accept a tagged target object. Examples:

    {"kind": "session", "session_id": 4}
    {"kind": "thread", "thread_id": 27}
    {"kind": "group", "group_id": 2}
    {"kind": "current_thread"}
    {"kind": "current_session"}
    {"kind": "session_set", "session_ids": [4, 7]}
    {"kind": "broadcast"}
    {"kind": "first"}
    {"kind": "multiple", "targets": [
      {"kind": "session", "session_id": 4},
      {"kind": "group", "group_id": 2}
    ]}

Global DDB thread and group IDs are used at this boundary. Backend-local IDs
never need to be discovered by the client.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| GET | /api/v1 | Service and active backend metadata |
| GET | /api/v1/capabilities | Discover resources, targets, controls, inspection, and DDB features |
| GET | /api/v1/health/live | Process liveness |
| GET | /api/v1/health/ready | Runtime component readiness |
| GET | /api/v1/state | One UI hydration snapshot |
| GET | /api/v1/sessions | Session resources |
| GET | /api/v1/groups | Distributed service groups |
| GET | /api/v1/breakpoints | Distributed breakpoint aggregates |
| POST | /api/v1/breakpoints | Create a session/group/multi-target breakpoint |
| PATCH | /api/v1/breakpoints/:id | Enable or disable a distributed breakpoint aggregate |
| DELETE | /api/v1/breakpoints/:id | Remove a distributed breakpoint |
| GET | /api/v1/commands/pending | Per-session command queue diagnostics |
| POST | /api/v1/commands | Full command vocabulary and backend escape hatch |
| POST | /api/v1/execution | Continue, interrupt, next, step-in/out, jump, or signal |
| POST | /api/v1/threads/query | Query threads with global IDs |
| POST | /api/v1/threads/select | Select a global thread |
| POST | /api/v1/stack/frames | Read frames for a global thread |
| POST | /api/v1/stack/variables | Read locals/arguments for a frame |
| POST | /api/v1/evaluate | Evaluate an expression |
| POST | /api/v1/memory/read | Read target memory bytes |
| GET | /api/v1/sources/resolve | Resolve a source to DDB service groups |
| GET | /api/v1/sources/content | Read a bounded window of debugger-known source |
| POST | /api/v1/ddb/distributed-backtrace | Run DDB's cross-service backtrace |
| GET | /api/v1/events | Subscribe to structured events over WebSocket |
| POST | /api/v1/shutdown | Gracefully stop DDB without stdin |

### Generic command

The generic endpoint guarantees parity as DDB's command vocabulary grows:

    curl -sS http://127.0.0.1:5000/api/v1/commands \
      -H 'content-type: application/json' \
      -d '{
        "command": "-data-list-register-values x",
        "target": {"kind": "thread", "thread_id": 27},
        "wait": true
      }'

wait defaults to true, yielding a structured completed receipt. With
wait: false, admission is bounded and the endpoint returns HTTP 202 with an
accepted receipt. Colon-prefixed diagnostic commands also use this shared
ingress.

### Execution

    {
      "action": "step_in",
      "target": {"kind": "thread", "thread_id": 27}
    }

Actions are continue, interrupt, next, step_in, step_out, jump, and
send_signal. jump additionally requires location; send_signal requires signal.

### Breakpoints

    {
      "source": "/workspace/src/server.rs",
      "line": 83,
      "target": {"kind": "group", "group_id": 2},
      "condition": "request.id == 42",
      "temporary": false,
      "hardware": false
    }

A group breakpoint remains a DDB aggregate: it is synchronized to current
members and inherited by later sessions in that service group.

`times` is the aggregate hit count across all current and past backend-local
targets represented by that DDB breakpoint. A hit publishes a
`breakpoint-modified` debugger record and a `BreakpointChanged` update before
the refreshed snapshot is read.

Enable or disable the aggregate and all of its current backend-local
breakpoints with:

    PATCH /api/v1/breakpoints/12
    {"enabled": false}

### Source content

    GET /api/v1/sources/content?path=/workspace/src/server.rs&start_line=50&end_line=150

Only paths reported by an active debugger group can be read. A response is
limited to 2,000 lines and the file must be a regular UTF-8 file no larger than
2 MiB. DDB binds to loopback by default; deployments that expose the control
plane remotely must place authentication and transport security in front of it.

## Capability-driven UI extensions

`GET /api/v1/capabilities` includes an `extensions` array. Each descriptor has
a stable extension id, human-facing title and description, and table panel
descriptors with stable panel ids and column labels. `GET /api/v1/state`
contains the matching dynamic rows:

    {
      "extensions": [{
        "id": "example.workers",
        "title": "Workers",
        "description": "Framework worker placement",
        "panels": [{
          "id": "placement",
          "title": "Placement",
          "columns": ["Worker", "Session"]
        }]
      }]
    }

    {
      "extensions": [{
        "id": "example.workers",
        "panels": [{
          "id": "placement",
          "rows": [["alpha", "7"]]
        }]
      }]
    }

Framework plugins own these descriptors and values. Core clients should render
known presentation shapes generically and ignore unknown extension or panel
ids. DDB's default/unspecified framework returns an empty extension list;
migration-specific proclet state is advertised only by frameworks that opt in
and have migration enabled.

## Event stream

Connect to ws://127.0.0.1:5000/api/v1/events. The first message is a welcome
record. Notifications then use the existing versioned envelope:

    {
      "version": 1,
      "timestamp": 1786620000,
      "notification_id": "d9fa253f-ac4d-481a-ae0e-3446de938479",
      "payload": {
        "type": "DebuggerOutput",
        "data": {
          "records": [{
            "stream": "exec",
            "event": "stopped",
            "payload": {
              "reason": "breakpoint-hit",
              "thread-id": "27",
              "session-id": "4"
            }
          }]
        }
      }
    }

Debugger and inferior output uses the same record envelope without MI string
escaping. For example:

    {
      "stream": "inferior_stdout",
      "event": "output",
      "payload": {"message": "request complete\n"}
    }

Stable stream names are `console`, `log`, `target`, `inferior_stdout`,
`inferior_stderr`, and `prompt`. Output records do not imply a state refresh;
lifecycle and breakpoint records do.

Other event types are BreakpointChanged, SessionStatusChanged,
SessionListChanged, and Custom. Clients should hydrate with /state, apply
events optimistically, and refresh after topology changes. Ping/pong heartbeats
remove stalled subscribers without blocking debugger progress.

## Compatibility and evolution

The legacy /send, /sessions, /groups, /group, /bkpts, /src_to_grp_ids,
/src_to_grps, /pcommands, /status, and /notifications/* routes remain available
with their prior wire shapes.

New clients should:

1. discover /api/v1/capabilities;
2. hydrate from /api/v1/state;
3. subscribe to /api/v1/events;
4. use typed endpoints for common operations;
5. use /api/v1/commands for commands not yet represented by a typed route.

Breaking wire changes require a new URL version. Additive fields and new
capabilities may appear within v1; clients should ignore unknown fields.
