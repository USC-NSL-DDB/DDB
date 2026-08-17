# DDB community API platform: implementation plan

Status: implementation-ready design and execution plan
Audience: DDB maintainers and coding agents implementing the public API platform
Scope: DDB backend API, event model, schemas, transports, SDKs, extension surface,
`ddb-tui` migration, compatibility, security, verification, and release governance

## 1. Purpose and outcome

DDB is becoming a debugger backend that can support many independently developed
frontends. The API must therefore be a first-class product boundary, not an HTTP
wrapper around stdin and not a contract inferred from `ddb-tui` implementation
details.

When this plan is complete, an external developer must be able to build a correct
frontend using only published schemas, generated or hand-written clients, public
documentation, and the conformance suite. That frontend must have access to the
same debugger semantics as stdin and `ddb-tui`, including DDB-specific distributed
features. No frontend may need to parse MI output, import DDB core modules, depend
on backend-local GDB/LLDB identifiers, or reproduce DDB routing and state logic.

This plan makes the following decisions:

1. DDB owns one transport-independent application service and one set of public
   semantics. HTTP, WebSocket, gRPC, Connect, stdin, and future DAP adapters are
   adapters over that service; they do not contain debugger business logic.
2. The current `/api/v1` HTTP/JSON and WebSocket API remains supported and is
   frozen as a compatibility adapter. Existing unversioned routes are frozen
   legacy adapters. Neither receives new feature design.
3. The refined contract is `ddb.api.v2`. A new major API version is necessary
   because opaque identifiers, resumable events, typed errors, operation handles,
   and consistent JSON/Protobuf mappings cannot be added without changing v1
   wire shapes.
4. Protobuf schemas are the canonical typed v2 interchange contract. They are
   independent of gRPC and are used to generate descriptors and language types.
5. HTTP/JSON plus replayable event streams remains a mandatory public binding.
   A browser, script, or community client must never be required to use gRPC.
6. gRPC/Tonic is the mature candidate for an optional native high-performance
   binding. It is not declared the default until DDB-specific measurements
   justify it.
7. Connect plus Protobuf is the preferred unifying transport candidate because
   one semantic service can support Connect, gRPC, and gRPC-Web with JSON or
   binary Protobuf. Its Rust implementation must pass the maturity and
   conformance gate in Stage 6 before becoming a production dependency.
8. OpenAPI and AsyncAPI are generated descriptions of the HTTP and event
   bindings. They improve discovery and tooling but do not dictate runtime
   transport or performance.
9. `ddb-tui` is the reference external consumer and conformance client. It must
   use the public SDK only and receive no private or privileged API behavior.
10. Performance choices are based on representative workloads, not on the
    assumption that binary serialization is always the bottleneck.

## 2. Current baseline

The implementation already has important foundations that must be preserved:

- `core/src/api/server.rs` exposes versioned HTTP routes and legacy adapters.
- `core/src/api/contract.rs` defines v1 request/response envelopes and target
  syntax.
- `core/src/api/read_model.rs` reads detached runtime snapshots instead of
  owning debugger state.
- `core/src/notification/` provides bounded WebSocket subscribers, heartbeat
  handling, and structured events.
- `core/src/cmd_flow/engine.rs` is shared by stdin and the API.
- `docs/runtime-architecture.md` establishes `RuntimeModel` as the exclusive
  mutable domain-state owner.
- `docs/debugger-backends.md` establishes backend-neutral command and event
  projection across GDB, LLDB, and Mock.
- `core/tests/api_v1.rs` verifies core v1 behavior against a real DDB process.
- `ddb-tui` is a separate Rust application using the public v1 HTTP/WebSocket
  boundary, including mock, GDB, and LLDB PTY workflows.
- `ddb-bench` measures the real DDB process boundary for API, CLI, notification,
  startup, and distributed-backtrace workloads.
- `core/Cargo.toml` already includes Tonic through the telemetry dependency set,
  but DDB has no public gRPC schema or service today; dependency presence is not
  evidence that the native API architecture has been implemented.

The current API is useful but not yet a durable community platform:

- Stable-looking endpoints still return arbitrary `serde_json::Value` command
  payloads for several typed operations.
- Public structs are defined inside the server crate rather than a publishable
  schema/types package.
- IDs use JSON numbers in v1, which is unsafe for JavaScript once values exceed
  its exact-integer range and conflicts with standard ProtoJSON `uint64` rules.
- The state snapshot has no event cursor. A reconnecting client can miss the
  race between hydration and subscription.
- Event IDs are UUIDs but there is no ordered sequence, bounded replay, gap
  response, state revision, or documented idempotent application rule.
- State events and high-volume output share subscriber infrastructure and do
  not have independently defined backpressure or loss policies.
- `wait: false` acknowledges command admission but exposes no durable operation
  handle or status query.
- Mutating requests do not have an explicit idempotency contract; blindly
  retrying `next`, `continue`, signal delivery, or breakpoint creation is unsafe.
- Capabilities are useful but not yet schema-versioned or detailed enough to
  express limits, backend restrictions, transport endpoints, permissions, and
  extension schemas.
- There are no checked-in Protobuf descriptors, OpenAPI/AsyncAPI artifacts,
  generated SDK release process, API-breaking-change gate, or third-party
  conformance runner.
- Remote exposure depends on an external proxy but the server does not yet
  enforce a fail-closed remote-binding policy.

The migration must strengthen these areas without weakening the runtime and
backend boundaries described in the existing architecture documents.

## 3. Requirements

Requirement identifiers are stable and must be referenced in implementation
PRs, test names where useful, and release checklists.

### R-01: one semantic implementation

- Every transport calls the same application service.
- Routing, target resolution, distributed breakpoints, execution ordering,
  global identity projection, source authorization, and DDB features remain in
  domain/application code.
- A transport adapter may decode, validate transport metadata, call the
  service, encode a response, and map errors. It may not implement debugger
  behavior or read mutable repositories directly.
- `RuntimeModel` remains the sole mutable debugger-state owner. A client-facing
  projection or event journal, if added, is derived and rebuildable; it is not
  a competing source of truth.

### R-02: stable, explicit, evolvable contracts

- All stable v2 requests, responses, resources, events, errors, capabilities,
  and extension envelopes are defined in versioned Protobuf packages.
- Additive evolution follows Protobuf compatibility rules. Removed fields and
  enum values are reserved and never reused.
- Unknown fields and unknown capabilities are tolerated by clients.
- Generated descriptors, HTTP JSON examples, OpenAPI, and AsyncAPI are
  reproducible from checked-in sources.
- Stable typed endpoints do not expose backend MI dictionaries or unrestricted
  JSON values. Dynamic values are confined to the explicitly unstable raw
  command and extension envelopes.

### R-03: compatibility and migration

- stdin behavior remains available and continues to use the shared command
  engine.
- `/api/v1` keeps its documented wire shape throughout the v2 rollout.
- Unversioned legacy routes are frozen: bug and security fixes only.
- v1 and v2 run concurrently until the release policy in Stage 9 is satisfied.
- The TUI supports negotiated v2 with an explicit v1 fallback during the
  migration window.
- No endpoint is silently repurposed and no existing response changes from a
  JSON number to string inside v1.

### R-04: complete debugger and DDB functionality

The public API covers, with typed contracts:

- service information, health, readiness, capabilities, and limits;
- sessions, groups, processes, threads, selection, and lifecycle;
- execution controls, including continue, interrupt, next, step-in, step-out,
  jump, signal, and backend-supported controls;
- breakpoint creation, update, enable/disable, deletion, conditions, hit
  counts, temporary/hardware flags, distributed aggregates, and group
  inheritance;
- frames, scopes, variables, lazy child expansion, expression evaluation,
  registers where supported, source resolution/content, and bounded memory;
- pending commands and asynchronous operation status;
- distributed backtrace, multi-target routing, context restore, framework
  features, and future DDB-specific features;
- console/inferior output and structured state events; and
- an explicitly labeled raw command escape hatch for parity while typed
  coverage evolves.

Capabilities must truthfully report backend and framework differences. An
unsupported operation fails explicitly; it never silently degrades.

### R-05: reliable snapshot and event synchronization

- A snapshot returns a state-event cursor and `base_state_revision` captured
  before snapshot collection.
- A client subscribes after that cursor. Events published during hydration are
  replayed, closing the hydration/subscription race.
- State events have a monotonically increasing sequence and `state_revision`
  scoped to a unique server-instance ID.
- State events are idempotent resource upserts/deletes or carry enough version
  information for a client to discard duplicates and stale updates.
- The server retains a bounded replay journal. When a cursor is too old or from
  another server instance, it returns a typed gap/out-of-range result directing
  the client to rehydrate.
- Reconnection behavior is deterministic and tested under duplicate, delayed,
  reordered-at-client, and missing-event simulations.

### R-06: explicit backpressure and resource limits

- State events and high-volume output use independent logical lanes and queues.
- A slow output consumer cannot delay state mutation, stop notification,
  command completion, stdin output, or another subscriber.
- All queues, replay journals, requests, payloads, source windows, memory reads,
  variable expansions, page sizes, operation histories, and connection counts
  are bounded.
- State-stream overflow disconnects with a resumable cursor where possible.
- Output-stream overflow reports an `OutputGap` with dropped message/byte counts
  or disconnects according to the advertised policy; it never blocks DDB.
- Limits are discoverable through capabilities and errors include the violated
  limit without exposing sensitive content.

### R-07: safe command semantics

- Each request has a server request ID; mutating operations also accept a
  client-generated idempotency key.
- The server maintains a bounded, expiring deduplication record so a retried
  mutation returns the original admission/result instead of executing twice.
- Every admitted mutation returns an operation ID. Operations expose
  accepted/running/completed/failed/cancelled states and typed per-target
  outcomes.
- A deadline limits how long the caller waits. It does not imply rollback of an
  already-admitted debugger command.
- Cancellation is exposed only for operations the command engine can actually
  cancel. Otherwise the server reports `not_cancellable`.
- Multi-target operations explicitly distinguish complete success, partial
  success, and complete failure. A single primary-session response is not used
  to hide partial outcomes.
- Execution-control completion semantics are documented: admission/backend
  acknowledgement is separate from the later running/stopped state event.

### R-08: transport choice without semantic drift

- HTTP/JSON and replayable WebSocket/SSE-style events are mandatory.
- A native binary binding may be enabled independently and must pass the same
  conformance vectors.
- gRPC/Protobuf, Connect/Protobuf, and HTTP/JSON measurements use identical
  application-service implementations and workload data.
- The transport decision is recorded in an ADR after Stage 6. gRPC is not made
  mandatory merely because Protobuf is canonical.
- DDB does not invent a raw socket RPC protocol, custom HTTP framing, or a new
  serialization format when an interoperable protocol satisfies the need.

### R-09: community extension model

- Core resources remain strongly typed.
- Extensions use namespaced, versioned descriptors and payload envelopes with
  an extension ID, schema URI, schema version, media type, and bytes or JSON
  payload.
- An extension declares capabilities, permissions, commands/actions, event
  types, and supported generic presentation shapes.
- Clients ignore unknown extensions and presentation shapes safely.
- Extension failure cannot corrupt core state or prevent core snapshot/event
  delivery.
- The UI descriptor vocabulary is deliberately small and versioned: table,
  tree, key/value, text, and action forms are sufficient initially. It is not
  an arbitrary remote UI execution system.

### R-10: usable SDK and documentation surface

- Publish a Rust SDK used by `ddb-tui`.
- Provide generated or generated-plus-thin-wrapper TypeScript and Python SDKs.
- SDKs implement capability negotiation, deadlines, typed errors, idempotency
  keys, pagination, snapshot-plus-replay, reconnect, and graceful shutdown.
- Examples demonstrate a minimal debugger client, event subscriber,
  breakpoint manager, and DDB distributed-backtrace consumer.
- A third-party conformance runner can validate a server or client without
  importing DDB internals.

### R-11: secure and operable defaults

- The default listener remains loopback-only.
- Binding to a non-loopback address fails unless the operator explicitly
  configures authentication plus TLS/reverse-proxy trust, or uses a clearly
  named insecure-development override.
- Authorization scopes distinguish read, control, and admin operations.
- CORS and browser origins are denied by default and use explicit allowlists.
- Source reads remain constrained to debugger-known paths. Memory, evaluation,
  raw commands, signals, and shutdown are treated as sensitive controls.
- Logs and traces include method, request ID, operation ID, target counts,
  status, duration, and sizes, but do not log source contents, memory contents,
  expressions, tokens, or arbitrary command text by default.
- Metrics cover method latency, failures, queue depth, subscriber count,
  replay use/gaps, dropped output, payload sizes, and operation lifecycle.

### R-12: backend parity and truthful behavior

- Mock, GDB, and LLDB use the same public resource/event semantics.
- Backend-specific limitations are represented in capabilities and typed
  `unsupported` errors.
- Backend-local identifiers never cross the stable API unless explicitly
  included in a diagnostic-only extension field.
- Real GDB and LLDB tests verify execution-line movement, independent source
  cursor behavior in the TUI, breakpoints, inspection, memory, evaluation,
  output, reconnect, and DDB-specific features.

### R-13: measurable quality

- Every stage has passing format, compile, lint, unit, integration, and
  compatibility gates before the next stage starts.
- Contract and generated-artifact diffs are reviewed like source code.
- Hot-path changes have before/after release benchmarks with raw JSON results.
- No performance claim is made from a single run or a generic serialization
  microbenchmark.
- Fuzz/property tests cover untrusted decoders and state/event convergence.

## 4. Non-goals

This roadmap does not:

- replace GDB/MI or LLDB's native bridge inside debugger backends;
- remove stdin or require existing automation to migrate immediately;
- make DAP the canonical DDB API—DAP may be a later adapter for IDE ecosystem
  compatibility but cannot express all DDB semantics;
- make UI layout or `ddb-tui` model structs part of the server contract;
- expose `RuntimeModel` repositories or transport-generated types inside the
  domain layer;
- guarantee replay of unbounded console/inferior output;
- guarantee cancellation or rollback for an operation already sent to a
  debugger;
- allow plugins to install arbitrary browser code through API descriptors;
- choose gRPC, Connect, CBOR, Cap'n Proto, FlatBuffers, Thrift, QUIC, or
  WebTransport without DDB-specific evidence; or
- combine unrelated runtime refactors with API migration commits.

## 5. Target architecture

### 5.1 Dependency direction

```text
GDB / LLDB / Mock
        |
        v
DebuggerProtocol -> command/domain services -> RuntimeModel
                              |
                              v
                    DdbApplicationService
                              |
                 public v2 contract types
             _________|___________
            |         |           |
      HTTP/JSON    gRPC/Connect   stdin/DAP
       + events      adapters      adapters
            |
            v
      generated SDKs
            |
       ddb-tui and community frontends
```

Allowed dependencies point downward in this diagram. In particular:

- domain/runtime code does not import Axum, Tonic, Connect, HTTP, WebSocket,
  OpenAPI, or generated client types;
- the application service may use API-owned request/response DTOs and converts
  between those and domain values;
- adapters depend on the service and contract, not on private managers; and
- SDKs depend only on published contract artifacts and transport libraries.

### 5.2 Planned repository shape

Names may change once crate publication names are checked, but responsibilities
must not be combined:

```text
ddb/
  proto/ddb/api/v2/
    common.proto
    resources.proto
    debugger_service.proto
    event_service.proto
    extension.proto
  api-types/                 # generated/public Rust messages and conversions
  api-client/                # publishable Rust client; no DDB core dependency
  api-conformance/           # black-box vectors and runner
  core/src/api/
    application/             # one service implementation
    transport/http_v1/       # existing v1 compatibility adapter
    transport/http_v2/       # v2 JSON binding
    transport/grpc/          # optional native adapter
    journal/                 # bounded state event replay
  docs/api/
    v2.md
    compatibility.md
    security.md
    extension-authoring.md
    generated/openapi-v2.yaml
    generated/asyncapi-v2.yaml
  tools/api-codegen/         # one deterministic generate/check entry point

ddb-tui/
  src/                       # consumes ddb-api-client only
```

Generated language outputs may live in release artifacts or dedicated SDK
repositories later. During implementation, generation configuration and golden
outputs must be available in this repository so CI can detect drift.

### 5.3 Public identifiers and scalar rules

- All public v2 IDs are opaque strings: session, group, process, thread,
  breakpoint, sub-breakpoint, operation, extension, server instance, and
  subscription IDs. Clients may compare and store them but must not calculate
  with them.
- The initial encoder may use decimal internal IDs, but that is not a contract.
- Target addresses remain strings because address width and syntax are target
  dependent.
- Memory contents use Protobuf `bytes`; JSON uses base64 according to the v2
  mapping. A human-facing hex rendering is an SDK/UI concern.
- Line and column values use unsigned integer fields with explicit one-based or
  zero-based documentation. Source lines are one-based.
- Optional scalar presence is explicit. Absence is never conflated with zero,
  empty string, or false.
- Timestamps use `google.protobuf.Timestamp`; durations use
  `google.protobuf.Duration`.
- Stable enums contain an `UNSPECIFIED = 0` value. Unknown values are preserved
  or handled as unknown, never treated as a known action.
- Large collections use pagination or bounded ranges. No list endpoint returns
  an unbounded process-wide collection.

### 5.4 Service shape

Use a small number of cohesive services rather than one RPC per backend
command. Exact method names may be refined in the schema ADR, but the semantic
coverage is required.

#### DebuggerService: metadata and reads

- `GetServerInfo`
- `GetCapabilities`
- `GetSnapshot`
- `ListSessions`, `GetSession`
- `ListGroups`, `GetGroup`
- `ListThreads`
- `ListFrames`
- `ListScopes`, `ListVariables`, `ExpandVariable`
- `ReadMemory`
- `ResolveSource`, `ReadSource`
- `ListBreakpoints`, `GetBreakpoint`
- `ListPendingCommands`
- `GetOperation`, `ListOperations`

`GetSnapshot` accepts explicit section selectors rather than an unrestricted
query language. Its response includes core topology, selections, breakpoints,
pending-operation summaries, extension state, `server_instance_id`, and the
`state_event_cursor` and `base_state_revision` captured before collection.
Expensive thread/frame/
variable/source data is requested separately or through bounded optional
snapshot sections to avoid both request waterfalls and enormous default
snapshots.

#### DebuggerControlService: mutations

- `Execute` for typed execution actions
- `SelectThread`
- `Evaluate`
- `CreateBreakpoint`, `UpdateBreakpoint`, `DeleteBreakpoint`
- `ExecuteRawCommand` as the compatibility escape hatch
- `RunDistributedBacktrace`
- typed future DDB feature operations
- `CancelOperation` where the capability says cancellation is supported

Every mutation accepts `idempotency_key`, target, and optional preconditions.
Every admitted mutation returns an `Operation`. The raw command result uses a
bounded neutral dynamic-value tree and is explicitly excluded from typed API
stability guarantees beyond its envelope and scalar/list/object representation.

#### DdbEventService: independent streams

- `SubscribeStateEvents(after_cursor, filters)`
- `SubscribeOutput(after_cursor?, filters)`

The HTTP binding exposes corresponding versioned event endpoints. State events
are replayable. Output replay is optional and bounded separately. The service
must not require a bidirectional stream for ordinary frontend operation;
control remains unary and event delivery remains server-streaming. This keeps
clients, proxies, failure recovery, and testing simpler.

#### DdbAdminService: operational and privileged lifecycle

- `GetHealth`
- `GetReadiness`
- `Shutdown`

Health and readiness expose only minimal operational state and may use an
unauthenticated or read scope according to deployment policy. `Shutdown` uses
the separate admin scope and is not mixed with general read/control access.

### 5.5 Error contract

Every transport maps one typed `DdbError` model:

- stable code enum;
- safe human-readable message;
- request and operation IDs when applicable;
- `retryable` boolean and optional retry delay;
- target/resource identifiers;
- field violations for invalid input;
- current/earliest cursor for replay gaps;
- required capability for unsupported behavior;
- structured per-target failures for fanout operations; and
- optional namespaced details using the extension envelope.

Required codes initially include `invalid_argument`, `not_found`, `conflict`,
`failed_precondition`, `unsupported`, `not_ready`, `unauthenticated`,
`permission_denied`, `resource_exhausted`, `deadline_exceeded`, `cancelled`,
`not_cancellable`, `replay_gap`, `backend_failed`, `partial_failure`,
`unavailable`, and `internal`.

HTTP status, gRPC status, and Connect status are mappings; they are not separate
error semantics. Internal errors receive a correlation ID and do not expose
backtraces, filesystem policy details, credentials, or debugger command text.

### 5.6 Operation model

`Operation` is a bounded application-service record, not a permanent audit log.
It contains:

- opaque operation ID and idempotency key hash/reference;
- request ID, operation kind, target summary, and initiating principal;
- accepted, started, and completed timestamps;
- state: accepted, running, completed, failed, or cancelled;
- typed result or error;
- per-target outcomes; and
- a state revision/cursor associated with any published result event.

Records expire after a documented configurable TTL and count/byte cap. Expired
lookups return a typed result. Sensitive request payloads are not retained.

### 5.7 State and event model

State-event envelopes contain:

- `server_instance_id`;
- monotonically increasing `sequence`, `state_revision`, and a serializable
  cursor;
- event schema version;
- occurrence timestamp;
- request/operation causation IDs when known;
- event kind;
- affected resource ID and resource revision;
- typed upsert/delete/change payload; and
- optional extension details.

Initial state event kinds include session/group/thread lifecycle, selection,
running/stopped, breakpoint upsert/delete, operation change, capabilities
change, extension-state change, and a required-resync marker.

Output envelopes contain their own sequence/lane, timestamp, session/thread
context when known, stream kind, text or bytes, and loss metadata. Console,
log, target, inferior stdout, inferior stderr, and prompt remain distinguishable.

Default journal policy proposed for the first implementation:

- state: retain until any of 10,000 events, 32 MiB, or five minutes is reached;
- output: no reconnect replay by default, with independently bounded per-client
  queues and explicit `OutputGap` reporting; and
- subscriber count: preserve the current safe default of 20 but make it
  configurable with a documented upper bound.

These are starting limits, not eternal wire guarantees. Advertise effective
limits through capabilities and record benchmark/memory evidence before
raising them.

The snapshot/replay algorithm is:

1. Read the current state-journal cursor and committed state revision with
   acquire ordering.
2. Collect a detached runtime snapshot using existing domain query boundaries.
3. Return the snapshot plus the cursor and base revision captured in step 1.
4. The client subscribes with `after_cursor`.
5. The journal replays all retained state events after that cursor and then
   transitions to live delivery without a gap.
6. Duplicate upserts are safe. If the cursor is unavailable or belongs to a
   prior server instance, return `replay_gap`; the client rehydrates.

Publication must occur after the corresponding domain mutation is committed.
If a snapshot observes the new state and later replays its event, resource
revision/idempotency rules make the duplicate harmless. Tests must prove this
ordering for activation, retirement, execution state, thread lifecycle, and
distributed breakpoint mutation.

### 5.8 Capability model

`Capabilities` is data, not a hard-coded UI menu. It includes:

- API/schema version and server instance;
- advertised transport endpoints and encodings;
- backend and framework identity;
- supported resources, operations, execution actions, breakpoint features,
  DDB features, cancellation semantics, and event kinds;
- effective page, payload, source, memory, replay, queue, operation, and
  subscriber limits;
- authentication mode and required scopes without disclosing secrets;
- extension descriptors and their schemas;
- deprecation notices with replacement and removal-not-before release; and
- optional performance hints such as preferred bounded page sizes.

Capabilities may change when sessions/backends/framework features change.
Publish a capabilities-change event and require clients to tolerate both
additions and removals.

### 5.9 Extension shape

An extension ID uses reverse-DNS or project-qualified naming, for example
`org.ddb.framework.proclets`. A descriptor includes owner, version, summary,
schema URI/hash, required scopes, commands/actions, emitted event types,
presentation descriptors, and compatibility range.

Extension state and events are carried as:

```text
ExtensionPayload {
  extension_id
  schema_version
  schema_uri
  media_type
  payload_bytes or payload_json
}
```

Core DDB validates size, declared media type, descriptor existence, and scope.
It does not claim to understand third-party payload semantics. Built-in DDB
extensions should still prefer typed messages registered in the main schema
when they are broadly useful.

### 5.10 Transport policy

#### Mandatory HTTP binding

- Maintain clear versioned paths under `/api/v2`.
- Use ordinary HTTP status codes plus the common typed error body.
- Support JSON for every unary method.
- Use standard compression only above measured thresholds.
- Publish OpenAPI generated from the mapping and contract.
- Publish AsyncAPI for HTTP event-stream/WebSocket bindings.
- Keep CORS disabled until explicitly configured.

#### Native binding

- Implement as a non-default feature or preview listener first.
- Tonic is the baseline mature Rust gRPC implementation.
- Reuse channels/connections and use unary calls for controls and reads.
- Use server streams only for long-lived state/output flows and chunked bulk
  reads where measurements justify them.
- Enable health and reflection for development/operations, subject to the same
  authentication policy.
- A separate preview port is acceptable initially. Stable discovery must
  advertise exact endpoints. Sharing one listener is preferred only if it
  remains simple, observable, and well tested.

#### Connect decision

Connect is eligible for production only when the selected Rust implementation:

- has an acceptable stability/MSRV policy and audited dependency story;
- passes the official conformance suite for the protocols DDB enables;
- supports unary and server streaming, deadlines, cancellation, compression,
  TLS, interceptors, and backpressure required by this contract;
- passes DDB's cross-transport conformance suite and fuzz tests;
- does not force domain/service code to depend on its runtime; and
- meets the benchmark and operational gates in Stage 6.

If it passes, prefer a multi-protocol server that serves Connect, gRPC, and
gRPC-Web semantics from the same generated service. If it does not, ship the
HTTP binding plus optional Tonic gRPC and reevaluate later. Do not maintain two
independent service definitions.

## 6. Required client workflow

A correct frontend follows this sequence:

1. Call `GetServerInfo` and `GetCapabilities`; reject unsupported API major
   versions and adapt to advertised backend/framework features.
2. Call `GetSnapshot` with only required sections and retain its
   `server_instance_id`, `state_event_cursor`, and `base_state_revision`.
3. Subscribe to state events after that cursor.
4. Apply replayed/live resource revisions idempotently. On `replay_gap`, clear
   derived client state and repeat steps 1–3.
5. Subscribe to output independently. Display `OutputGap` rather than implying
   complete output when loss occurs.
6. Submit mutations with a fresh idempotency key, retain the returned operation
   ID, and do not interpret admission as a stopped-state transition.
7. Observe operation and running/stopped events or query operation status after
   reconnect.
8. Use typed methods whenever available. Use the raw command endpoint only
   after capability discovery and without assuming backend MI wire shapes.
9. Render known extension descriptors and ignore unknown extension IDs/shapes.
10. Treat IDs as opaque and avoid auto-retrying a mutation without the same
    idempotency key.

The Rust SDK must implement this workflow so `ddb-tui` does not reproduce it.

## 7. Staged implementation

Each stage must land with its exit criteria passing. Do not begin a later
transport or SDK migration while an earlier semantic stage is incomplete.

### Stage 0: establish a reviewable baseline

#### Rationale

The current working tree contains the v1 API, runtime refactor, debugger
backend work, and the new `ddb-tui` as uncommitted or branch-local changes.
Building a multi-stage API migration on an ambiguous baseline would make
regressions and authorship impossible to audit.

#### Work

1. Inventory all current changes with `git status`, branch history, and the
   existing review handoff.
2. Either land the current v1/TUI/runtime work in its already intended logical
   commits or start this roadmap from a commit where that work is present.
   Never sweep unrelated dirty files into an API-platform commit.
3. Run all existing correctness gates and the ignored TUI PTY suite with Mock,
   GDB, and LLDB.
4. Capture release benchmark JSON for the existing scenarios and record host,
   compiler, debugger versions, configuration, samples, and raw results.
5. Freeze representative v1 HTTP responses, errors, WebSocket messages, and
   stdin outputs as golden compatibility fixtures.
6. Add a requirements traceability table mapping R-01 through R-13 to planned
   tests and stages.

#### Exit criteria

- Baseline commit is identified and reproducible.
- Workspace and TUI gates pass.
- Raw benchmark and compatibility fixtures are checked in under a dated
  baseline directory.
- No roadmap commit contains unrelated pre-existing changes.

### Stage 1: characterize and freeze compatibility

#### Rationale

Refactoring transports before freezing behavior makes accidental API changes
indistinguishable from intended v2 work.

#### Work

1. Expand v1 black-box tests to every documented route, error envelope, target
   kind, capability field, event kind, source limit, shutdown, and legacy route.
2. Add stdin/API semantic parity vectors for threads, execution, breakpoints,
   stack, variables, evaluation, memory, sources, and distributed backtrace.
3. Add malformed JSON, oversized body, unknown field, empty target, invalid
   path, missing session/thread, and partial multi-target failure tests.
4. Extend `ddb-bench` with representative frontend workloads:
   - step/next submission to stopped-event latency;
   - snapshot hydration at 1/16/64 sessions;
   - 10,000-variable or equivalent large hierarchical inspection data;
   - 1, 16, and 64 MiB memory transfer using bounded chunks;
   - sustained stdout/stderr delivery;
   - 1/8/20 subscribers, including a deliberately slow consumer;
   - reconnect plus replay/resync once implemented; and
   - mixed small control calls during large output/memory traffic.
5. Record allocation/CPU/wire-byte metrics in addition to latency percentiles.

#### Exit criteria

- The current v1 and stdin contracts are protected by black-box fixtures.
- Missing current behavior is documented as a known limitation, not silently
  invented in the tests.
- New benchmark scenarios run against the real DDB process and emit structured
  JSON suitable for before/after comparison.

### Stage 2: define the canonical v2 schema and governance

#### Rationale

Schema design must precede adapters. Otherwise the first transport library
accidentally becomes the architecture.

#### Work

1. Add `proto/ddb/api/v2` using Protobuf Editions or the currently supported
   project-wide syntax selected in an ADR. Do not mix syntax styles casually.
2. Define common IDs, targets, pagination, errors, operations, resources,
   capabilities, events, extension envelopes, and service methods described in
   Section 5.
3. Add comments to every public message, field, enum, method, unit, bound,
   idempotency rule, and permission requirement.
4. Add `buf` formatting, lint, and breaking-change configuration. Compare
   against the last released descriptor set, not merely the current branch.
5. Generate and check in a deterministic descriptor set and Rust types.
6. Add encode/decode golden vectors for binary Protobuf and ProtoJSON, including
   unknown fields, optional presence, opaque IDs, bytes, large integers, and
   unknown enum values.
7. Write API compatibility and deprecation policy documents.
8. Record ADRs for v2 versioning, Protobuf selection, ID representation,
   service boundaries, and dynamic extension containment.

#### Exit criteria

- `buf format`, lint, and breaking-change gates pass.
- Descriptor and generated Rust output reproduce without a diff.
- JavaScript/TypeScript and Python can consume golden vectors correctly.
- No stable typed response contains an unbounded `Struct`, `Any`, or JSON value
  except the explicitly documented raw/extension envelope.
- v1 fixtures remain unchanged.

### Stage 3: introduce the transport-independent application service

#### Rationale

All transports must share behavior before another transport is added.

#### Work

1. Create `DdbApplicationService` with query, control, event, and admin ports.
2. Move target validation, request bounds, command/result projection, source
   policy, partial-result construction, operation admission, and typed error
   construction out of Axum handlers into the service.
3. Keep domain mutations in existing command services and `RuntimeModel`.
   Do not move mutable domain ownership into the API layer.
4. Introduce API-owned conversion modules from domain snapshots and neutral
   debugger records to v2 contract types.
5. Adapt v1 handlers to call the new service and translate results back to the
   exact frozen v1 shapes. Keep legacy handlers as translations over the same
   service/command engine.
6. Add a fake/in-memory service harness for adapter contract testing without a
   debugger, while retaining real-process integration tests.
7. Implement operation IDs, bounded status storage, idempotency deduplication,
   deadlines, and partial target outcomes in the application layer.
8. Instrument request and operation IDs across service calls and command/event
   causation without logging sensitive payloads.

#### Code-review constraints

- Axum handlers should be thin and have no backend command string construction
  except inside a named compatibility translator.
- No new manager getter is added to `RuntimeModel`.
- No repository lock is held across `await`.
- New queues and caches have count and byte limits plus shutdown behavior.
- An idempotency cache key is scoped to server instance and principal/client
  context so unrelated clients cannot retrieve each other's results.

#### Exit criteria

- v1 golden fixtures and real integration tests are byte/shape compatible.
- stdin behavior remains unchanged.
- Application-service unit tests exercise all typed operations independent of
  HTTP.
- Repeating a mutation with the same idempotency key executes it once; using a
  new key executes a new mutation.
- Expiry, capacity, deadline, shutdown, and partial-failure paths are tested.

### Stage 4: build the resumable state/event plane

#### Rationale

Correct state synchronization and backpressure matter more to an interactive
debugger than switching JSON to a faster codec.

#### Work

1. Introduce a single ordered state-event journal after committed domain
   mutations. Assign server instance, sequence, cursor, resource revision, and
   causation metadata.
2. Audit every client-visible mutation path: activation, retirement, group
   membership, thread lifecycle/selection, running/stopped, breakpoint
   aggregate/sub-breakpoint changes, operation lifecycle, framework extension
   state, and capability changes.
3. Split state events and output into independent bounded lanes.
4. Implement snapshot cursor capture, replay-to-live handoff, typed replay-gap
   response, heartbeat, and graceful shutdown.
5. Make upsert/delete event application idempotent and document resource
   revision comparison.
6. Add slow-consumer policy, output gap records, metrics, and configurable
   bounded retention.
7. Preserve the v1 WebSocket stream through a translator. It need not gain v2
   replay semantics, but it must continue receiving the same v1 event shapes.
8. Add concurrency/property tests that generate mutations while clients
   hydrate, disconnect, replay, and reconnect.

#### Required invariants

- A state event is never published before its domain mutation commits.
- Replay and live delivery have no sequence gap or overlap bug; duplicate
  delivery is allowed and harmless.
- Per-lane sequences are monotonic within a server instance.
- A restarted server never accepts an old cursor as current.
- A full/closed subscriber queue never blocks debugger progress.
- State-event loss is never silently represented as complete state.
- Retirement and group-breakpoint ordering continue to satisfy
  `runtime-architecture.md`.

#### Exit criteria

- Deterministic tests cover all mutation paths and replay boundaries.
- A client hydrating concurrently with thousands of state changes converges to
  the same projection as a fresh snapshot.
- Slow output clients do not regress control/stopped-event latency beyond the
  performance gate.
- Memory use remains bounded at all configured journal/subscriber limits.

### Stage 5: expose and document the v2 HTTP/event binding

#### Rationale

The universally accessible binding should be complete before a native-only
binding becomes attractive to the first-party TUI.

#### Work

1. Implement `/api/v2` unary JSON routes as thin application-service adapters.
2. Implement v2 state and output subscriptions with cursor/filter support.
3. Use the common v2 error, operation, pagination, capability, and limits
   models everywhere.
4. Enforce content type, body size, decompression ratio, page/window limits,
   deadline, and connection limits before expensive work.
5. Generate OpenAPI and AsyncAPI through one checked-in codegen command.
6. Add runnable curl and browser/TypeScript examples.
7. Add black-box conformance tests that compare HTTP responses/events with
   application-service golden vectors.
8. Keep v1/v2/legacy routes live in the same test process and verify isolation.

#### Exit criteria

- All v2 methods required by R-04 are available or explicitly capability-gated.
- OpenAPI/AsyncAPI regenerate without a diff and validate in CI.
- Example clients run against Mock DDB.
- JSON decoder fuzzing and malicious size/compression tests pass.
- v1 fixtures remain unchanged.

### Stage 6: implement native transport previews and make an evidence-based decision

#### Rationale

Protobuf does not require gRPC, and DDB's current mock API latency indicates
that backend work, fanout, refresh patterns, and event delivery may dominate.
The native transport must earn its complexity on real frontend workloads.

#### Work

1. Add an optional Tonic adapter generated from the canonical v2 schema. Use
   unary RPCs for ordinary methods and server streams for state/output.
2. Add reflection, health, deadlines, compression thresholds, message limits,
   authentication interceptors, graceful shutdown, and metrics.
3. Prototype the current Connect Rust implementation behind a non-default
   feature or isolated spike. Run its official conformance suite and document
   MSRV, dependency, maintenance, and Protobuf-runtime implications.
4. Run identical workload data through:
   - v2 HTTP/JSON and event binding;
   - Tonic gRPC/Protobuf; and
   - Connect JSON/Protobuf and gRPC compatibility if the spike passes basic
     correctness.
5. Measure p50/p95/p99 latency, throughput, CPU, allocations, resident memory,
   wire bytes, reconnect behavior, slow-subscriber impact, and operational
   complexity.
6. Test 1/16/64 sessions, large variable trees, memory chunks, high output,
   multiple subscribers, and mixed control/bulk traffic.
7. Write a transport ADR selecting one of:
   - HTTP/JSON mandatory plus optional Tonic gRPC;
   - a production Connect multi-protocol server plus the documented HTTP
     resource binding; or
   - HTTP/JSON only until native transport evidence improves.

#### Decision thresholds

These thresholds are policy defaults and may be changed only in the ADR with
raw evidence:

- No transport may weaken correctness, replay, security, or compatibility for
  performance.
- A native binding should show a material advantage in at least one target
  workload: at least 20% lower CPU/allocation/wire cost or a user-visible p95
  latency/throughput improvement that cannot be obtained by removing request
  waterfalls or chunking.
- Small unary controls may regress no more than both 10% and 0.25 ms p95 versus
  the stage baseline; both the relative and absolute threshold must be exceeded
  before failing to reduce noise.
- Existing 16-session burst scenarios may regress no more than both 10% and
  1 ms p95.
- Stopped-state events must not be starved by bulk streams; mixed-workload p95
  must remain within the same gate as isolated control traffic after accounting
  for documented debugger work.
- Compare at least three repeated runs on the same host and use the median of
  run percentiles. Keep all raw output.

#### Exit criteria

- Cross-transport conformance vectors are identical at the semantic level.
- The ADR records measured evidence, dependency maturity, browser/tooling
  impact, operational costs, and the chosen default/optional status.
- Any rejected preview is removed cleanly or remains explicitly experimental
  and non-default; it does not become accidental permanent architecture.

### Stage 7: publish SDKs and migrate `ddb-tui`

#### Rationale

The first-party frontend is the strongest proof that the public contract is
complete, but migrating it earlier would hide missing HTTP functionality by
encouraging privileged native APIs.

#### Work

1. Create `ddb-api-client` with transport abstraction, typed methods, typed
   errors, capability negotiation, pagination, idempotency, operation polling,
   and snapshot/replay reconnection.
2. Make HTTP/JSON the baseline client feature. Add the selected native feature
   without changing the high-level SDK API.
3. Move all URL construction, JSON parsing, WebSocket reconnect, and event
   convergence out of `ddb-tui` into the SDK.
4. During migration, negotiate v2 first and allow an explicit v1 fallback.
   Report which protocol is active in diagnostics.
5. Verify that source cursor and execution location remain independent: `▶`
   follows frame-zero stopped execution, `▸` remains navigation, and `●`
   remains breakpoint state.
6. Add TypeScript and Python generation/wrappers with minimal debugger,
   event/reconnect, breakpoint, and distributed-backtrace examples.
7. Add packaging metadata, version compatibility table, changelog generation,
   and a release dry run without publishing.
8. Ensure SDK source depends only on public contracts; no DDB core path
   dependency or private type import is allowed.

#### Exit criteria

- `ddb-tui` has no hand-written v2 wire DTOs or route strings outside an
  explicitly isolated v1 fallback module.
- Mock/GDB/LLDB PTY tests pass through the public SDK.
- A forced disconnect during step/output/topology changes converges without
  losing the execution marker or duplicating a mutation.
- TypeScript and Python examples pass against the same Mock DDB conformance
  fixture.
- SDK packages can be built reproducibly and their compatibility range is
  documented.

### Stage 8: harden extensions, security, and operations

#### Rationale

Community extensibility without namespacing, permissions, limits, and
observability becomes an unstable security boundary.

#### Work

1. Replace the table-only implicit extension contract with the versioned
   descriptor/envelope registry while preserving a v1 translation for current
   built-ins.
2. Add schema registration, collision detection, payload limits, permission
   requirements, action dispatch, event registration, and presentation-shape
   validation.
3. Add a sample out-of-tree extension and a sample generic frontend renderer.
4. Implement loopback/non-loopback startup policy, bearer/token-file or
   equivalent initial authentication, read/control/admin scopes, TLS/proxy
   trust configuration, and explicit insecure-development override.
5. Add CORS/origin allowlists, request rate/concurrency limits, decompression
   guards, sensitive-log redaction, and audit events for control/admin actions.
6. Add health/readiness behavior per listener, metrics, tracing propagation,
   graceful drain, and subscriber/operation diagnostics.
7. Threat-model source reads, memory, evaluation, raw commands, extension
   payloads, replay storage, reflection, shutdown, and remote deployment.
8. Consider a Unix-domain-socket binding for same-host native clients only
   after the portable transports are stable and benchmark evidence supports it.

#### Exit criteria

- Non-loopback insecure startup fails unless the explicit development override
  is present.
- Authorization tests cover every service method and extension action.
- Secrets and sensitive debugger payloads are absent from default logs/traces.
- Fuzz, rate-limit, oversized payload, slow-loris/slow-consumer, replay-memory,
  and graceful-drain tests pass.
- The sample extension can be consumed without changes to `ddb-tui` core logic.

### Stage 9: stabilize, publish, and govern compatibility

#### Rationale

An API becomes a community contract through predictable release and support,
not merely through generated code.

#### Work

1. Run a release candidate with v1 and v2 enabled and gather compatibility,
   performance, reconnect, and extension feedback.
2. Publish versioned docs, descriptors, OpenAPI/AsyncAPI, SDKs, conformance
   runner, examples, migration guide, security guide, and transport ADR.
3. Add API/schema change templates requiring compatibility classification,
   migration notes, test updates, and benchmark impact.
4. Establish support windows:
   - v1 remains supported through v2 general availability and for at least two
     documented release cycles afterward;
   - no removal occurs before a release announced in capabilities and the
     changelog;
   - unversioned legacy removal requires a major DDB release and measured usage
     or explicit operator migration acknowledgement; and
   - schema/SDK versions declare their supported server API range.
5. Add deprecation telemetry that records route/method counts without payloads.
6. Publish contributor instructions for adding a method, field, event,
   capability, extension, transport mapping, and conformance vector.
7. After v2 conformance is stable, build an out-of-process DAP adapter as an
   ecosystem proof using only the public SDK. Map conventional debugger features
   to standard DAP and expose DDB-only behavior through namespaced custom
   requests/events; do not make DAP a second semantic implementation.

#### Exit criteria

- A clean environment can build a functional frontend from published artifacts
  and docs alone.
- The conformance runner passes HTTP and every supported native binding.
- v1 and stdin regression suites pass unchanged.
- Release artifacts are reproducible and signed according to project policy.
- Deprecation/removal dates and replacements are discoverable by clients.

## 8. Quality and verification guidelines

### 8.1 Required local correctness gates

Run from `ddb/` after every applicable commit:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
```

After the new crates exist, add explicit gates for `ddb-api-types`,
`ddb-api-client`, and `ddb-api-conformance`. Run from `ddb-tui/` when SDK or
frontend behavior changes:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

Run the ignored PTY suite for release candidates and any contract, event,
execution, inspection, breakpoint, source, SDK, or reconnect change:

```bash
cargo build -p ddb --manifest-path ../ddb/Cargo.toml
cargo build --manifest-path ../ddb/core/tests/fixtures/real_loop/Cargo.toml
cargo test --test e2e_mock -- --ignored
```

Record GDB and LLDB versions with the result.

### 8.2 Schema and generated-artifact gates

The codegen tool added in Stage 2 must provide one `generate` mode and one
`--check` mode. CI must enforce:

- Protobuf formatting and lint;
- breaking comparison against the last released descriptor set;
- deterministic Rust, TypeScript, Python, descriptor, OpenAPI, and AsyncAPI
  generation;
- no generated diff after `--check`;
- binary and JSON golden-vector compatibility; and
- public Rust SDK semantic-version checks once published.

A generated file change without the source schema/generator change that caused
it is a review failure.

### 8.3 Test layers

1. **Unit tests:** conversions, validation, target resolution, errors,
   pagination, limits, operation/idempotency behavior, cursor parsing, and
   extension validation.
2. **Contract goldens:** every request/response/error/event in binary Protobuf,
   ProtoJSON, HTTP JSON, and v1 compatibility form.
3. **Application-service tests:** all semantics without a transport.
4. **Adapter conformance:** the same vector through HTTP, gRPC, and Connect if
   supported.
5. **Real-process integration:** DDB with Mock for determinism and with GDB/LLDB
   for backend truth.
6. **TUI PTY:** rendered state, source/execution markers, keyboard/mouse,
   terminal restoration, and reconnect.
7. **Property tests:** snapshot plus any valid retained event suffix converges;
   duplicates are harmless; stale resource revisions never overwrite newer
   ones; pagination is complete without duplicates.
8. **Fuzz tests:** JSON/Protobuf/event/cursor/dynamic-value decoders, extension
   envelopes, compressed bodies, and malformed streams.
9. **Load/soak tests:** repeated reconnect, slow clients, high output, operation
   cache churn, journal rollover, session activation/retirement, and shutdown.
10. **Security tests:** auth scopes, CORS, remote bind, source path, size/rate
    limits, log redaction, reflection/admin exposure, and malicious inputs.

### 8.4 Performance methodology

- Benchmark release builds of the real `ddb` binary, not isolated handler
  functions.
- Pin scenario configuration, samples, warmup, compiler, debugger, and host.
- Use at least three repeated runs and retain all JSON output.
- Evaluate p50/p95/p99, CPU time, allocations, RSS, throughput, and wire bytes.
- Separate startup, command admission, backend execution, response projection,
  event delivery, and UI-observed stop latency when possible.
- Compare identical semantic payloads across transports.
- Use large real-shaped variable/source/memory/output payloads; a ping-pong
  benchmark is insufficient.
- Treat a result as a regression only when it exceeds both the relative and
  absolute budgets in Stage 6 or is repeatable outside expected noise.
- If a regression is accepted, document the correctness/usability gain and
  update the recorded budget intentionally.

### 8.5 Review checklist

Every API review answers:

- Which requirement IDs does this change implement?
- Is behavior in the application/domain layer or accidentally in an adapter?
- Is the change additive, compatible, conditionally compatible, or breaking?
- Are field numbers/names reserved correctly?
- Are IDs opaque and optional values explicit?
- Are errors stable and useful without leaking internals?
- Are retry, idempotency, deadline, cancellation, and partial-failure semantics
  defined?
- Can any queue, cache, list, request, response, or replay buffer grow without
  a count and byte bound?
- Can a slow/disconnected client block debugger progress?
- Does capability discovery report the behavior and limit truthfully?
- Are HTTP/native/SDK/docs/conformance artifacts updated together?
- Do v1, stdin, Mock, GDB, LLDB, and TUI tests remain correct?
- Is benchmark evidence required and attached?
- Are security scopes and sensitive logging addressed?

## 9. Commit and PR plan

### 9.1 Commit discipline

- Use one branch/PR per stage unless a stage is explicitly split below.
- Keep every commit buildable and testable. Feature flags and dormant adapters
  are preferable to a long broken series.
- Do not mix formatting, dependency upgrades, runtime refactors, generated
  output, and feature behavior unless they are inseparable.
- Preserve existing user changes; resolve overlap rather than resetting or
  rewriting unrelated work.
- Schema source and the generated output needed to keep the build green land in
  the same commit. Generator implementation may land immediately before it.
- Compatibility fixtures change only in an explicitly labeled breaking-version
  commit with an ADR. v1 fixtures should not change in this roadmap except to
  add missing coverage or correct a documented bug.
- Every commit body includes:

```text
Requirements: R-xx, R-yy
Why: <semantic reason>
Compatibility: <none/additive/v2-only/v1-preserved>
Tests: <exact commands and notable cases>
Benchmarks: <artifact path or not-applicable reason>
```

- PR descriptions include the same information, the generated/contract diff,
  risk and rollback plan, and before/after benchmark links.
- If repository policy squashes PRs, preserve the logical commit list and test
  evidence in the PR description and release notes.

### 9.2 Planned logical commits

The names below are the expected slices. A coding agent may split a slice
further to keep reviews small but must not combine adjacent semantic stages.

#### PR 0: baseline and plan

1. `docs(api): add community API platform implementation plan`
2. `test(api): freeze v1 and stdin compatibility fixtures`
3. `bench(api): capture frontend workload baseline`

Artifacts: this plan, traceability table, golden fixtures, dated raw benchmark
JSON, environment manifest.

#### PR 1: compatibility characterization

1. `test(api): expand v1 route error and event goldens`
2. `test(api): add stdin and api semantic parity vectors`
3. `bench(api): add frontend scale bulk and slow-client workloads`
4. `docs(api): record baseline limitations and traceability`

Artifacts: complete v1/stdin fixtures, expanded benchmark scenarios, known-gap
register, and requirement-to-test traceability.

#### PR 2: schema foundation

1. `build(api-schema): add deterministic protobuf toolchain`
2. `feat(api-schema): define ddb api v2 common and resource types`
3. `feat(api-schema): define debugger and event services`
4. `test(api-schema): add binary and json compatibility vectors`
5. `ci(api-schema): enforce lint generation and breaking checks`
6. `docs(api): record v2 schema and compatibility decisions`

Artifacts: proto sources, descriptor set, generated types, ADRs, golden vectors.

#### PR 3: application-service boundary

1. `refactor(api): introduce transport-independent application service`
2. `feat(api): add typed errors operations and idempotent admission`
3. `refactor(api): adapt v1 handlers through the shared service`
4. `test(api): prove v1 and stdin compatibility after service refactor`

Rollback: v1 adapter can be switched back to the previous handler wiring while
the service remains dormant; no v1 contract fixture changes are allowed.

#### PR 4: state and output journals

1. `feat(api-events): add ordered bounded state event journal`
2. `feat(api-events): split state and output backpressure lanes`
3. `feat(api-events): add snapshot cursors replay and gap handling`
4. `test(api-events): verify mutation ordering and replay convergence`
5. `bench(api-events): measure replay and slow-subscriber behavior`

Rollback: disable v2 journal consumers while retaining existing v1 translated
notifications; domain mutation paths must remain independent.

#### PR 5: v2 HTTP and specifications

1. `feat(api-http): expose v2 typed json endpoints`
2. `feat(api-http): expose resumable state and output streams`
3. `build(api-docs): generate openapi and asyncapi artifacts`
4. `test(api-http): add black-box v2 conformance suite`
5. `docs(api): add v2 examples migration and reconnect guides`

Rollback: v2 routes are independently mountable; v1 and stdin remain active.

#### PR 6: native transport preview and ADR

1. `feat(api-grpc): add optional tonic v2 adapter`
2. `test(api-grpc): run shared conformance vectors and stream failures`
3. `experiment(api-connect): evaluate rust connect multi-protocol support`
4. `bench(api): compare json grpc and connect frontend workloads`
5. `docs(adr): select ddb native transport policy`
6. `chore(api): remove or mark rejected preview dependencies`

The Connect experiment may remain a non-merged spike if its dependency
maturity gate fails. Its ADR and raw evidence still land.

#### PR 7: Rust SDK and TUI migration

1. `feat(api-client): add public rust client and reconnect state machine`
2. `test(api-client): add server and transport conformance tests`
3. `refactor(ddb-tui): consume the public ddb api client`
4. `feat(ddb-tui): negotiate v2 with explicit v1 fallback`
5. `test(ddb-tui): verify mock gdb lldb and reconnect workflows`

Rollback: retain the isolated v1 client feature until v2 general availability.

#### PR 8: TypeScript/Python SDKs and conformance runner

1. `feat(api-sdk): generate typescript and python clients`
2. `feat(api-conformance): add black-box third-party runner`
3. `test(api-sdk): run cross-language golden and mock workflows`
4. `docs(api): add community frontend tutorials`

#### PR 9: extension platform

1. `feat(api-extensions): add namespaced schema registry and envelopes`
2. `feat(api-extensions): add bounded generic presentation and actions`
3. `refactor(api-extensions): translate existing framework panels`
4. `test(api-extensions): add out-of-tree sample extension`
5. `docs(api-extensions): publish authoring and compatibility guide`

#### PR 10: security and operations

1. `feat(api-security): enforce safe bind and authentication policy`
2. `feat(api-security): add read control and admin authorization`
3. `feat(api-ops): add limits metrics tracing and graceful drain`
4. `test(api-security): add threat-model regression suite`
5. `docs(api): publish deployment and security guidance`

#### PR 11: stabilization and release

1. `test(api): run release compatibility and soak matrix`
2. `perf(api): record v2 release candidate benchmark evidence`
3. `docs(api): publish v1 to v2 migration and support policy`
4. `chore(release): package schemas sdks conformance and examples`
5. `docs(release): record api v2 general availability`

### 9.3 Audit artifacts retained per stage

Each stage retains:

- baseline and final commit hashes;
- exact test commands and machine-readable results;
- schema/descriptor compatibility report;
- generated-artifact check result;
- raw benchmark JSON plus environment manifest;
- ADRs and rejected alternatives;
- security/threat-model changes where applicable;
- migration/rollback notes; and
- requirement-to-test traceability updates.

Do not retain credentials, source/memory payloads, or sensitive debugger output
inside benchmark and test artifacts.

## 10. Global definition of done

The roadmap is complete only when all statements below are true:

1. A third party can discover capabilities, hydrate state, subscribe/reconnect,
   control execution, manage breakpoints, inspect source/stack/variables/memory,
   and invoke DDB distributed features from public artifacts alone.
2. `ddb-tui` uses the public Rust SDK and has no privileged backend path.
3. HTTP/JSON is complete and supported. Any native transport is optional or
   multi-protocol according to the recorded ADR.
4. Protobuf schema, descriptors, OpenAPI, AsyncAPI, Rust/TypeScript/Python
   outputs, examples, and conformance vectors reproduce in CI.
5. Snapshot plus replay converges under concurrent mutations and reconnect;
   stale cursors and server restarts force explicit rehydration.
6. State and output backpressure are bounded and cannot block debugger work.
7. Mutating retries are idempotent, operation status is queryable, and partial
   multi-target outcomes are explicit.
8. Stable v2 types contain no accidental backend MI or arbitrary JSON leakage.
9. Extension descriptors are namespaced, versioned, permissioned, bounded, and
   safely ignorable.
10. Default local use remains easy; remote exposure is fail-closed and
    documented.
11. v1, legacy, stdin, Mock, GDB, LLDB, SDK, TUI, schema, fuzz, security, load,
    and performance gates pass with retained evidence.
12. Compatibility/deprecation governance and support windows are published.
13. Every implementation PR can be traced to requirements, tests, benchmark
    evidence, generated diffs, and a rollback boundary.

## 11. References informing transport decisions

- [Protocol Buffers overview](https://protobuf.dev/overview/)
- [Protocol Buffers language and evolution guidance](https://protobuf.dev/programming-guides/proto3/)
- [ProtoJSON format](https://protobuf.dev/programming-guides/json/)
- [Tonic / grpc-rust](https://github.com/grpc/grpc-rust)
- [gRPC performance guidance](https://grpc.io/docs/guides/performance/)
- [gRPC flow control](https://grpc.io/docs/guides/flow-control/)
- [Connect protocol](https://connectrpc.com/docs/protocol/)
- [Connect multi-protocol support](https://connectrpc.com/docs/multi-protocol/)
- [Connect Rust implementation RFC](https://connectrpc.com/docs/governance/rfc/007-rust-implementation/)
- [CBOR Internet Standard](https://www.rfc-editor.org/rfc/rfc8949.html)
- [Cap'n Proto RPC](https://capnproto.org/rpc.html)
- [FlatBuffers design](https://flatbuffers.dev/white_paper/)
- [WebTransport working draft](https://www.w3.org/TR/webtransport/)

These references guide implementation choices but do not replace DDB-specific
conformance, security, and benchmark evidence.
