# DDB public API changelog

This changelog covers the canonical v2 schema, generated contract artifacts,
public SDKs, and transport bindings. DDB runtime changes that do not alter the
public API belong in the project release notes.

## Unreleased

### Added

- Canonical `ddb.api.v2` Protobuf schema, descriptor set, ProtoJSON mappings,
  generated Rust types, OpenAPI, and AsyncAPI documents.
- Transport-independent debugger application service with typed errors,
  opaque IDs, bounded pagination, idempotent mutation admission, and retained
  asynchronous operations.
- Replayable, revisioned state events and an independently backpressured output
  stream with explicit gap reporting.
- Stable HTTP/ProtoJSON binding and opt-in Tonic gRPC preview.
- Typed `ListSignals` discovery with backend-reported stop, print, pass, and
  description metadata; signal delivery remains a separate typed mutation.
- Per-execution-action scope capabilities so frontends can distinguish thread,
  session, group, and fanout controls without probing by failure.
- `ddb-api-client` with capability negotiation, bounded page collection,
  operation polling, SDK-owned state projection/reconnect, and output reconnect.
- Deterministically generated TypeScript and Python ProtoJSON types and method
  registries, plus bounded frontend SDKs with negotiation, deadlines,
  idempotency, pagination, operation polling, reconnect, snapshot/replay, and
  graceful shutdown.
- Minimal inspection, event, breakpoint, and distributed-backtrace examples in
  both community SDKs.
- `ddb-api-conformance`, a public-SDK-only black-box verifier with read-only and
  deterministic Mock profiles, plus real-process Rust/TypeScript/Python gates.
- Reproducible Rust crate, npm package, and Python wheel release dry runs and an
  explicit SDK/server compatibility table.
- A descriptor-plus-policy operation registry that generates the runtime Axum
  route/authorization/error/stream binding together with OpenAPI, AsyncAPI, and
  SDK method tables, backed by standards, live-payload, route-identity, and
  three-surface compatibility gates.

### Fixed

- HTTP/ProtoJSON now uses the canonical comma-separated string mapping for
  `google.protobuf.FieldMask`, matching generated TypeScript and Python
  clients.
- `BreakpointSpec.enabled` now has Protobuf presence. Create omission defaults
  to enabled, masked updates require a value, and resource responses preserve
  explicit `false` in ProtoJSON.
- Breakpoint updates can atomically request `enabled` and `condition` changes
  at the logical-resource boundary, with rollback of debugger-local copies on
  partial backend failure.
- Disabled breakpoints are installed atomically on Mock, GDB, and LLDB instead
  of briefly becoming enabled before a follow-up command.
- Partially successful distributed breakpoint creation and deletion retain the
  debugger-local copies that still exist and return them as a typed operation
  result alongside deterministic per-target failures.
- The LLDB bridge now implements condition, temporary, and enable/disable
  breakpoint behavior, while hardware breakpoints are truthfully omitted from
  LLDB capabilities and rejected with typed `UNSUPPORTED`.
- The deterministic Mock backend now reflects enabled and conditional
  breakpoint state and exposes standard `-break-list` output, so conformance
  tests can detect debugger-local state drift.
- Sensitive memory reads now consistently require CONTROL permission in HTTP,
  gRPC, generated specifications, and conformance tests.
- Plain JSON `404` responses can enable an explicit v1 migration fallback, but
  valid typed v2 resource-not-found errors, authentication errors, malformed
  responses, and transport failures cannot trigger downgrade.
- TypeScript event iterators cancel their underlying response reader on early
  return, preventing a live stream socket from surviving frontend shutdown.
- Distributed-backtrace frame results now carry ordinary inspectable frame
  identities, while synthetic call boundaries remain explicitly non-inspectable.

### Compatibility

- API v1 and the MI-shaped stdin interface remain available alongside v2.
- API v2 remains preview status at schema baseline `2.0.0-draft.3`; no general
  availability or v1 removal date has been declared.
