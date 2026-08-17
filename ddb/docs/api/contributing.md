# Contributing to DDB API v2

The public API is a community contract. Add semantics once in
`DdbApplicationService`, describe them in the canonical Protobuf schema, then
adapt transports and SDKs to that same behavior.

## Choose the right extension point

- Add a core method or field when the concept is backend-neutral and broadly
  useful to debugger frontends.
- Add a namespaced extension when a framework owns the semantics or needs to
  evolve independently. Follow [`extension-authoring.md`](extension-authoring.md).
- Use the raw-command method only as a temporary compatibility escape hatch; do
  not make its dynamic result a new stable contract.
- Keep presentation/layout decisions in frontends. Server descriptors expose
  only the small generic table, tree, key/value, text, and action vocabulary.

## Adding a field, message, enum, or method

1. Read [`compatibility.md`](compatibility.md) and the ADRs. Classify the change
   as additive, conditionally compatible, or breaking before editing.
2. Edit `proto/ddb/api/v2`. Document units, presence, bounds, idempotency,
   permission scope, errors, and capability behavior. Use opaque string IDs,
   an `UNSPECIFIED = 0` enum value, and pagination/bounded windows.
3. Never reuse a released field or enum number. Reserve removed names and
   numbers. Do not add `Any`, `Struct`, or arbitrary JSON to stable resources.
4. Add the application behavior under `core/src/api/application`. Target
   resolution, command construction, state publication, and partial outcomes do
   not belong in Axum, Tonic, or an SDK.
5. Add capability and limit discovery before a frontend relies on the behavior.
   Unsupported backends must return typed `UNSUPPORTED`; they must not degrade
   silently.
6. Regenerate all contract artifacts in one change:

   ```bash
   cargo run -p ddb-api-codegen -- generate
   cargo run -p ddb-api-codegen -- --check
   ```

   The generator owns Rust/TypeScript/Python types, the descriptor set,
   OpenAPI, AsyncAPI, and method registries. Do not edit generated files.
7. Ensure HTTP and optional gRPC call the same application method and use the
   same typed error. Add the method to the generated scope classifier; unknown
   methods deliberately fail code generation.
8. Expose the high-level operation in the Rust SDK and generated-plus-thin
   TypeScript/Python SDKs without importing DDB core. Add or update an example
   when this is a new frontend workflow.
9. Add unit, application-service, ProtoJSON/binary golden, HTTP black-box,
   native conformance, SDK, and real-backend tests in proportion to the change.
   Event changes also need replay/convergence and slow-consumer coverage.
10. Update `CHANGELOG.md`, compatibility/migration documentation, OpenAPI and
    AsyncAPI descriptions, conformance vectors, and
    [`traceability.md`](traceability.md).

## Event and operation rules

Publish state events only after the domain mutation commits. Give each event a
server instance, sequence, state revision, resource revision, and causation IDs
when known. A new event must be an idempotent upsert/delete or carry enough
version information to reject stale delivery. State and output remain separate
bounded lanes.

Every admitted mutation accepts an idempotency key and returns an operation.
Its record must have bounded retention, truthful cancellation semantics, and
explicit per-target outcomes. A deadline limits caller waiting; it does not
roll back debugger work already admitted.

## Permission and sensitive-data review

Assign each method READ, CONTROL, ADMIN, or deliberately public. Memory,
evaluation, raw commands, signals, and extension actions are sensitive even if
their response appears observational. Add the method to the exhaustive
generated-route authorization test.

Telemetry may record static method/route, request and operation IDs, status,
duration, counts, and byte sizes. It must not record tokens, authorization
headers, raw commands, expressions, source paths/content, memory, output text,
or extension payloads. Bound every request, list, cache, queue, journal, and
response before expensive work.

## Required gates

From `ddb/`:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
./tools/check-api-release.sh
```

For a contract decoder change, run both targets documented in `fuzz/README.md`.
The scheduled fuzz workflow is supplemental; deterministic regression inputs
belong in normal tests after minimization.

For frontend-affecting changes, also run from `ddb-tui/`:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
cargo test --test e2e_mock -- --ignored
```

The ignored PTY suite requires the sibling DDB and real-loop fixture binaries,
GDB, and LLDB as described in the TUI README.

## Review and commit evidence

An API pull request must identify requirement IDs, compatibility class, exact
tests, generated diffs, security/limit impact, benchmark impact, migration and
rollback boundaries, and traceability updates. Follow the logical commit plan
in `community-api-platform-implementation-plan.md`; do not mix unrelated
runtime changes or rewrite frozen v1 fixtures to hide a regression.
