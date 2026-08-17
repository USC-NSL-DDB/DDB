# DDB community API requirement traceability

This index maps the stable roadmap requirements to implementation boundaries,
authoritative tests, and release gates. It is maintained with the contract; a
green narrow test is not evidence for a broader requirement.

| Requirement | Implementation evidence | Verification evidence |
|---|---|---|
| R-01 one semantic implementation | `core/src/api/application`; v2 HTTP and Tonic adapters call `DdbApplicationService`; runtime mutation remains in `RuntimeModel`/command services | application tests in `service.rs`; HTTP/gRPC semantic comparison in `api_v2_grpc.rs`; dependency checks in SDK manifests |
| R-02 explicit contracts | `proto/ddb/api/v2`; `ddb-api-types`; deterministic generator and descriptor/OpenAPI/AsyncAPI outputs | `api-types/tests/contract.rs`; codegen `--check`; Buf lint/breaking CI; TypeScript/Python tests |
| R-03 compatibility | frozen v1/legacy routers with a named compatibility command translator over the shared engine/read model; isolated `ddb-tui/src/legacy_v1.rs` fallback | `api_v1.rs`; legacy routing/execution suites; v1 fallback PTY case; v1/v2 route-isolation security tests; schema changelog |
| R-04 complete functionality | 43 typed service methods plus raw escape hatch, capabilities, extensions, and operations; breakpoint create/delete plus masked enable/condition updates share the logical distributed-breakpoint service | exhaustive v2 process test; real GDB/LLDB create/update/delete parity including disabled creation; injected multi-target create/delete partial-failure and update-rollback tests; public conformance Mock profile; generated method registry |
| R-05 synchronization | snapshot checkpoint, bounded state journal, revisioned resource events, replay-aware SDK projection | journal 5,000-mutation convergence/property cases; `api_v2_client.rs`; release-only rollover/rehydration soak; replay/gap/restart projection tests |
| R-06 bounded backpressure | separate state journal and output hub, startup-validated `Conf.ApiLimits`, bounded request/encoded-response/pages/source/memory/variables/operations/subscribers | slow-subscriber/gap tests; mixed unread-output/control benchmark; exact response-bound unit test; NDJSON line bounds; configured-capability process test; request admission/security tests |
| R-07 safe commands | principal-scoped idempotency store, operation lifecycle, deadlines, preconditions, per-target outcomes, retained typed results for partial resource mutations | operation-store tests; targeting/partial-failure process tests for execution and breakpoint create/delete/update; idempotent HTTP and SDK tests |
| R-08 transport consistency | mandatory HTTP/ProtoJSON, feature-gated Tonic adapter over the same service | `api_v2_grpc.rs`; generated contract checks; equivalent HTTP/gRPC snapshot and typed step-to-stop scenarios; [ADR 0005](adr/0005-transport-policy.md); three retained snapshot benchmark runs |
| R-09 extensions | versioned public `ddb-api-extension` registry, bounded envelopes/schemas/actions, generic renderer | extension crate tests; sample out-of-tree extension test; application extension test; TUI renderer tests |
| R-10 SDK/docs | Rust, TypeScript, Python SDKs/examples and public-SDK-only conformance runner | SDK unit/process tests; language SDK process test; reproducible release dry run |
| R-11 security/operations | fail-closed remote bind, token grants/scopes, origin/rate/concurrency/body policies, safe HTTP/gRPC and bounded-store telemetry, graceful drain | exhaustive 43-method authorization test; deployment/CORS/rate/shutdown process tests; capture-based sensitive-log test; auth/security/limit unit tests; gRPC process test |
| R-12 backend parity | backend-neutral projections and explicit capability/unsupported behavior; LLDB condition/temporary/enable behavior and hardware limitation are projected truthfully; SDK-only TUI | real GDB/LLDB API breakpoint create/list/update/delete tests; Mock partial-fanout/rollback/fidelity tests; Mock/GDB/LLDB TUI PTY suite; typed execution-line movement benchmark; optimized-build thread-state regression; distributed-backtrace backend tests |
| R-13 measurable quality | CI schema/workspace/security/extension/SDK/TUI/soak gates, optimized-build state-transition gate, standalone public-decoder fuzz targets, deterministic packaging, retained raw transport and frontend-workload evidence | `.github/workflows/rust-check.yml`; `.github/workflows/api-fuzz.yml`; `api_v2_soak.rs`; `tools/check-api-release.sh`; three-run snapshot, typed-control/output, 10k-variable, 1/16/64 MiB memory, 1/8/20-subscriber, and 1/16/64-session reconnect evidence with hashes and environment manifests |

## Release-candidate evidence checklist

The following results are environment- and revision-specific and must be
captured for each release candidate rather than declared permanently complete:

- clean-tree format, all-target/all-feature compile, strict lint, workspace and
  DDB test output;
- Mock, GDB, and LLDB versions plus the complete TUI PTY output;
- Buf formatting/lint/breaking comparison against the last published
  descriptor, deterministic generation, and OpenAPI/AsyncAPI validation;
- Rust/TypeScript/Python package hashes from the release dry run;
- fuzz/property and bounded soak/load results for changed untrusted decoders,
  journals, streams, operation storage, and shutdown paths;
- before/after benchmark evidence for hot-path changes; and
- confirmation that v1 and stdin compatibility fixtures did not change unless
  an explicitly approved compatibility decision says otherwise.

The transport snapshot evidence currently retained under
`benchmarks/evidence/2026-08-14-v2-transport` covers only the decision described
by ADR 0005. It is not baseline evidence for CPU, allocations, memory,
streaming, large payloads, or mixed traffic.

The control/output evidence retained under
`benchmarks/evidence/2026-08-14-v2-control-output` covers typed HTTP/gRPC
step-to-stop correctness and paired drained/unread output behavior at 1/16/64
Mock sessions. It does not supply CPU, allocation, RSS, complete wire-byte, or
real-backend performance evidence.

The frontend-workload evidence retained under
`benchmarks/evidence/2026-08-14-v2-inspection-replay` covers 10,000-variable
paging, bounded 1/16/64 MiB memory transfer, 1/8/20-subscriber stop fanout, and
SDK reconnect/replay convergence at 1/16/64 Mock sessions. It carries the same
resource-metric and real-backend limitations.

The complete local gate results, tool/debugger versions, fuzz execution counts,
and release-only qualifications for this working tree are retained in
[`release-readiness-2026-08-14.md`](release-readiness-2026-08-14.md).
