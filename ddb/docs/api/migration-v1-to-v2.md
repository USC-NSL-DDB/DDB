# Migrating a frontend from DDB API v1 to v2

API v2 is additive: it runs beside the frozen v1 and unversioned compatibility
routes. Migrate one frontend boundary at a time; do not reinterpret v1 IDs or
response fields as v2 values.

## Support window

V1 remains supported through v2 general availability and for at least two
documented DDB release cycles afterward. A removal release must be announced in
the changelog and `Capabilities.deprecations` with a replacement. Unversioned
legacy routes require a major DDB release plus measured migration evidence or
explicit operator acknowledgement before removal. No v1 or legacy removal date
has been announced.

## Behavioral changes

| V1 | V2 |
|---|---|
| Numeric JSON IDs | Opaque strings scoped to one server instance |
| `{api_version,data,error}` envelope | Canonical ProtoJSON response or typed `DdbError` |
| Hydrate, then open one WebSocket | Snapshot cursor, replayable state stream, independent output stream |
| A waited command response | Admission `Operation`, then operation/running/stopped events or polling |
| Retrying a mutation is unsafe | Retry only with the same client idempotency key |
| MI-shaped command result | Typed resources/results; bounded raw-command escape hatch only |
| Implicit backend behavior | Capability-gated behavior and typed `UNSUPPORTED` errors |
| One combined event queue | Independently bounded state and high-volume output lanes |

V2 uses standard ProtoJSON: 64-bit integers are decimal strings, bytes are
base64, enum names are symbolic, and omitted optional fields remain absent.
Use generated types or an SDK instead of hand-written JSON models.

## Route and method mapping

All unary v2 methods are `POST` requests under
`/api/v2/rpc/ddb.api.v2.<Service>/<Method>`.

| V1 route | V2 method or workflow |
|---|---|
| `GET /api/v1` | `DebuggerService.GetServerInfo` |
| `GET /api/v1/capabilities` | `DebuggerService.GetCapabilities` |
| `GET /api/v1/health/live` | `DdbAdminService.GetHealth` |
| `GET /api/v1/health/ready` | `DdbAdminService.GetReadiness` |
| `GET /api/v1/state` | `DebuggerService.GetSnapshot` |
| `GET /api/v1/sessions` | `ListSessions` / `GetSession` |
| `GET /api/v1/groups` | `ListGroups` / `GetGroup` |
| `POST /api/v1/threads/query` | `ListThreads` / `GetThread` |
| `POST /api/v1/threads/select` | `DebuggerControlService.SelectThread` |
| `POST /api/v1/stack/frames` | `DebuggerService.ListFrames` |
| `POST /api/v1/stack/variables` | `ListScopes`, `ListVariables`, `ExpandVariable` |
| `POST /api/v1/evaluate` | `DebuggerControlService.Evaluate` |
| `POST /api/v1/memory/read` | `DebuggerService.ReadMemory` (`CONTROL`) |
| `GET /api/v1/sources/resolve` | `DebuggerService.ResolveSource` |
| `GET /api/v1/sources/content` | `DebuggerService.ReadSource` using the opaque source reference |
| v1 breakpoint routes | `List/Get/Create/Update/DeleteBreakpoint` |
| `POST /api/v1/execution` | `DebuggerControlService.Execute` |
| `POST /api/v1/ddb/distributed-backtrace` | `RunDistributedBacktrace` |
| `POST /api/v1/commands` | Prefer a typed method; otherwise `ExecuteRawCommand` |
| `GET /api/v1/commands/pending` | `ListPendingCommands` and operation methods |
| `GET /api/v1/events` | `SubscribeStateEvents` plus `SubscribeOutput` |
| `POST /api/v1/shutdown` | `DdbAdminService.Shutdown` (`ADMIN`) |

Process resources, registers, execution state, extension schemas/actions, and
operation history are first-class v2 capabilities without one-to-one v1
routes.

## Required v2 startup and synchronization flow

1. Call `GetServerInfo` and `GetCapabilities`; reject an unsupported API major.
2. Request only needed `GetSnapshot` sections. Retain the server instance,
   state cursor, and base revision.
3. Subscribe to state events after that cursor and apply only newer resource
   revisions. Duplicates are harmless.
4. On `REPLAY_GAP`, discard the derived projection and hydrate again.
5. Subscribe to output separately and display `OutputGap` as presentation loss.
6. Give every mutation a fresh idempotency key. Retain its operation ID and
   observe or poll until terminal.
7. Treat admission, target running state, and a later stopped event as distinct
   transitions.

The Rust, TypeScript, and Python SDKs implement these mechanics. Prefer them to
duplicating reconnection, pagination, deadline, and error logic.

## Controlled fallback

A client may negotiate v2 first and expose an explicit v1 fallback during the
migration window. A missing v2 discovery route (`404`) may enable the fallback;
authentication, authorization, malformed response, timeout, and server errors
must not silently downgrade. Diagnostics must show the active protocol, and
the v1 adapter must keep numeric IDs and non-replayable events isolated.

`ddb-tui --api-version v1-fallback` is the reference policy. New frontends
should normally omit fallback and require v2.

## Migration verification

Run the public conformance runner against a disposable DDB before enabling v2:

```bash
cargo run -p ddb-api-conformance -- \
  --endpoint http://127.0.0.1:5000 \
  --token "$DDB_API_TOKEN" --output json
```

For a Mock deployment with a CONTROL token, add `--profile mock` to exercise
mutations. Test forced reconnect, stale-cursor rehydration, operation retry with
the same key, output gaps, unsupported capabilities, and process restart before
removing the v1 adapter from a frontend.
