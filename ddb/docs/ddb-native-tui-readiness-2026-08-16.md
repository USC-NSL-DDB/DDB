# DDB-native TUI gap-closure readiness

Date: 2026-08-16

Status: implementation and end-to-end verification complete. The work is recorded as five allowlisted local commits described below; no remote branch was updated while preparing this report.

This report closes the functional and semantic review against the prototype in /home/ybyan/projs/vscode-adapter. It supplements the earlier integrated usability report and focuses on one product rule:

> ddb-tui is the first-party terminal frontend for DDB, not a conventional single-process debugger TUI with DDB features bolted on.

## Result

The TUI now treats DDB topology, distributed call stacks, distributed breakpoints, typed signals, extension surfaces, and target-scoped execution as primary workflows. It still uses only the public DDB v2 SDK and wire contract, so the first-party implementation remains a conformance client rather than a privileged backend peer.

The public contract was tightened where frontend correctness depends on server truth:

- ordinary distributed frames carry normal, inspectable frame IDs;
- synthetic transition boundaries are visibly distinct and non-inspectable;
- distributed result truncation and its reason survive from operation result to UI;
- execution capabilities declare supported actions and scopes;
- breakpoint UI requires both the relevant feature and mutation operation;
- typed signal discovery replaces guessed platform signal lists;
- fanout receipts preserve per-target failures;
- generic extension descriptors, state, schemas, and actions avoid framework-specific core UI code;
- schema version 2.0.0-draft.3 and all generated artifacts describe the same contract.

## Prototype comparison

| Prototype behavior | Prior gap or misalignment | Current DDB-native behavior |
|---|---|---|
| Distributed call-stack provider | Distributed backtrace was a manual secondary action | Every stopped-thread inspection uses DDB distributed backtrace as the primary stack; d only refreshes it |
| Cross-session separator rows | Boundary and debugger frames were conflated | Boundaries remain explanatory rows; ordinary owner frames remain selectable and inspectable |
| Grouped sessions | TUI presented a mostly flat thread list | Topology is group → session → thread, including ungrouped and temporarily unresolved groups |
| Session/group controls | Controls implicitly followed one selected thread | The displayed execution target is independently selectable and cycles through thread, session, group, and broadcast |
| Group/session breakpoint picker | Breakpoint creation targeted only the current context | Picker supports server-resolved broadcast, inheritable groups, individual sessions, and explicit multi-target sets |
| Aggregate breakpoint presentation | Distributed child state was flattened | Manager keeps the aggregate and per-session sub-breakpoint verification, pending, hit, condition, ignore, and diagnostic state |
| Session-focused signal UI | Signals could be raw commands or guessed names | TUI requests DDB's typed signal catalog and sends the selected typed signal to the selected DDB target |
| Focused distributed frame navigation | Caller ownership could be lost | Frame selection carries owner thread/session into source, locals, and register inspection |
| DDB-specific framework panel | Prototype contained framework-shaped UI assumptions | Public extension descriptors/state/actions render generically; no framework appears unless advertised |
| Session kill button | Prototype implemented this as SIGINT | Intentionally rejected: interrupt is not termination, and DDB currently advertises no typed session-terminate operation |

The TUI also goes beyond the prototype with cursor-based event replay, independent output streaming, managed authenticated backend ownership, compatibility negotiation, capability-filtered controls, lazy variable trees, register formats, generic extension actions, responsive mouse hit testing, and terminal/lifecycle recovery tests.

## Requirement closure

| ID | Requirement | Implemented behavior | Quality evidence |
|---|---|---|---|
| NATIVE-01 | Distributed backtrace is the default stack | Selecting or refreshing a stopped thread runs typed RunDistributedBacktrace automatically | Unit projection tests; mock and real GDB/LLDB PTY workflows |
| NATIVE-02 | Cross-session frames remain useful | Ordinary distributed rows retain owner session/thread and inspectable v2 frame IDs; boundaries are non-inspectable | API v2 projection tests; real depth-1/depth-4 GDB/LLDB tests |
| NATIVE-03 | Bounded results are honest | Truncation reason is retained, summarized in receipts, shown in status/timeline, and persists as a TRUNCATED stack title | State and render regressions |
| NATIVE-04 | DDB topology is first-class | Group/session/thread tree includes partial readiness, ungrouped sessions, unresolved group metadata, state, and location | Model/render tests; distributed managed PTY case |
| NATIVE-05 | Execution targeting is explicit | Group/session selection changes execution scope without stealing inspected-thread selection; c cycles scopes | Model tests and capability rejection tests |
| NATIVE-06 | Controls follow server capabilities | Unsupported actions/scopes are hidden or rejected before queueing | Typed capability tests and group-step negative test |
| NATIVE-07 | Breakpoints are distributed resources | Broadcast/group/session/multiple targeting, group dominance normalization, aggregate/sub-breakpoint state, enable/delete gating | Model, UI, API targeting, and PTY tests |
| NATIVE-08 | Inspection is typed and ownership-safe | Lazy locals/arguments, registers, source, evaluation, and memory use opaque public resource IDs | API client, model, mock, GDB, and LLDB tests |
| NATIVE-09 | Signals are discoverable DDB operations | Typed signal catalog carries stop/print/pass/description and target | API, model, and mouse render tests |
| NATIVE-10 | DDB extensions are public and generic | Descriptor-driven panels plus schema-validated typed actions require no framework switch in TUI core | Sample extension service, model, and render tests |
| NATIVE-11 | Async state cannot steal focus | Revisioned events update topology; background stops do not replace the active inspection; stale responses are ignored | Main-loop race regressions and reconnect PTY case |
| NATIVE-12 | Execution and navigation are different | Source gutter uses ▶ for execution, ▸ for cursor, and ● for breakpoint; navigation remains pinned independently | Render regression and exact post-step GDB/LLDB PTY assertions |
| NATIVE-13 | First-party use is streamlined | ddb tui CONFIG and direct ddb-tui CONFIG own an authenticated private backend; distributed configs need no manual backend startup | Managed dispatcher/config/PTY and lifecycle cases |
| NATIVE-14 | Community clients use the same path | ddb-tui imports public API crates and uses HTTP/ProtoJSON plus state/output streams with no ddb-core shortcut | Cargo dependency audit, public client integration, conformance tests |

## API contract shape

The protobuf files under ../proto/ddb/api/v2 are the semantic source of truth. Generated Rust types, OpenAPI, AsyncAPI, and gRPC bindings must reproduce from that source. HTTP/ProtoJSON is the transport used by ddb-tui today; gRPC is an optional transport over the same messages and service semantics, not a second debugger model.

Frontend authors should follow these rules:

1. Negotiate GetCapabilities and schema compatibility before exposing controls.
2. Treat IDs as opaque. Never infer a session, thread, frame, or group by parsing an ID.
3. Hydrate a bounded snapshot, then consume revisioned state events from the acknowledged cursor.
4. Consume output independently from state so console backpressure cannot delay stop events.
5. Treat a distributed boundary as presentation metadata, not as an inspectable frame.
6. Use the owner thread/frame supplied by each ordinary distributed row for inspection.
7. Keep an aggregate operation receipt and its per-target results; partial fanout failure is not total success.
8. Let the server resolve broadcast and group inheritance rather than expanding a stale frontend snapshot.
9. Render optional framework behavior only from advertised extension descriptors and state.
10. Fail closed on malformed protocol, permission failure, identity change, or incompatible schema; do not silently downgrade.

The current first-party managed workflow requires v2. Explicit v1 fallback remains external-connect-only and degraded: it cannot provide cursor replay or the complete native model.

## User-interface semantics

The layout is intentionally DDB-shaped:

- left: group/session/thread topology plus aggregate distributed breakpoints;
- center: source plus DDB distributed call stack;
- right: lazy variables, registers, memory, generic extensions, and DDB timeline;
- top: capability-gated execution controls, target scope, and connection state;
- bottom: status, prompt, and contextual hints.

Mouse and keyboard routes converge on the same model commands. A group or session row is a valid execution target without pretending it is an inspectable thread. A thread row selects the inspection root. A distributed frame row selects its real owning frame. A boundary row explains the causal/session transition and cannot dispatch frame inspection.

Breakpoint target selection is explicit. Broadcast is mutually exclusive. Group selection absorbs redundant member-session selections. Multiple independent groups/sessions become a typed multiple target only when distributed breakpoints are advertised.

## Deliberately narrowed surface

The following were not copied from the prototype or invented in the TUI:

- no session “kill” implemented by sending SIGINT;
- no reverse controls when DDB advertises no reverse operation;
- no cancellation UI while cancellable_operation_kinds is empty;
- no set-variable mutation without a typed mutation contract;
- no function/address breakpoint creator or ignore-count editor when those mutations are not advertised and consistently enforced;
- no source-path-to-group heuristic; v2 uses typed group/session targets and server-side routing;
- no framework-specific proclet code in the default UI;
- no automatic/manual backend split for ordinary users: managed configuration is the default, while connect is reserved for an externally owned service.

Raw GDB/MI remains an explicit expert escape hatch. It is backend-specific, permission-gated, and must not be treated by community clients as the portable DDB API.

## Validation evidence

All commands below completed successfully against this working tree.

DDB API/runtime:

    cargo fmt --manifest-path ddb/Cargo.toml --all --check
    cargo check --manifest-path ddb/Cargo.toml --workspace --all-targets --all-features
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb --all-targets --all-features --no-deps -- -D warnings
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb-api-types --all-targets --no-deps -- -D warnings
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb-api-client --all-targets --all-features --no-deps -- -D warnings
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb-api-grpc --all-targets --all-features --no-deps -- -D warnings
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb-api-conformance --all-targets --no-deps -- -D warnings
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb-api-extension --all-targets --no-deps -- -D warnings
    cargo clippy --manifest-path ddb/Cargo.toml -p ddb-sample-extension --all-targets --no-deps -- -D warnings
    cargo test --manifest-path ddb/Cargo.toml --workspace --all-targets
    cargo run --manifest-path ddb/Cargo.toml -p ddb-api-codegen -- --check
    cargo test --manifest-path ddb/Cargo.toml -p ddb --test api_v2_client
    cargo test --manifest-path ddb/Cargo.toml -p ddb --test real_distributed_backtrace -- --test-threads=1

The full DDB workspace check and test gates were green; the core library ran 307 passing tests with one intentional ignored test. Generated API artifacts reproduced exactly. The public Rust client integration passed 1/1. Real distributed backtrace passed 4/4 across GDB and LLDB at local and cross-session depths. Strict Clippy passed for DDB and every maintained public API/extension crate. The deliberately broader workspace Clippy command still reports the documented legacy `gdbmi` and dependency-metadata lint backlog, so this report does not claim that unrelated gate passes.

ddb-tui:

    cargo fmt --manifest-path ddb-tui/Cargo.toml --all --check
    cargo test --manifest-path ddb-tui/Cargo.toml --all-targets
    cargo clippy --manifest-path ddb-tui/Cargo.toml --all-targets --all-features -- -D warnings
    cargo build --manifest-path ddb-tui/Cargo.toml --release
    cargo test --manifest-path ddb-tui/Cargo.toml --test e2e_mock -- --ignored --test-threads=1

The normal suite passed 73/73. The ignored PTY matrix passed 16/16 serially, covering deterministic mock, delayed connection, explicit v1 migration, managed dispatcher/config, distributed partial readiness, credential isolation, backend crash/panic/signal/terminal-loss cleanup, real GDB and LLDB launch, real GDB attach, execution markers, source/stack/variables/registers/memory, breakpoints, mouse input, and terminal restoration. The optimized ddb-tui artifact was built and is executable.

Repository hygiene also passed:

- git diff --check over ddb and ddb-tui;
- no stale pre-draft.3 schema references outside build output;
- no .rej or .orig patch artifacts;
- no generated API drift.

## Auditable commit ledger

The originally mixed working tree was separated by allowlisted path staging. Each local commit was inspected with `git diff --cached --check` and records requirements, rationale, compatibility, and exact tests in its body. Review the series in this order:

1. `feat(api): establish the public DDB v2 platform`

   DDB runtime/application changes, the canonical v2 protobuf contract, generated specifications and bindings, public Rust/TypeScript/Python SDKs, extensions, examples, and core/API tests.

2. `feat(tui): add the first-party DDB-native frontend`

   The standalone `ddb-tui` crate, including its model, public-API integration, rendering, managed backend lifecycle, DDB-native workflows, and mock/real PTY coverage.

3. `test(api): add release gates and performance evidence`

   API-focused CI and review gates, bounded decoder fuzzing, reproducible package checks, benchmark methodology, and retained evidence.

4. `docs(ddb): publish native frontend and community contracts`

   Root navigation, architecture and implementation plans, the integrated user guide, release notes, and both readiness ledgers.

5. `build(release): package the paired DDB binaries`

   Reproducible paired-binary packaging plus an extracted, empty-`PATH` smoke test. This boundary follows the documentation commit because those documents are deliberate archive inputs.

The first four subjects above are fixed at the time this report is committed; the fifth is the immediately following packaging boundary. Use `git log --format=fuller` and `git show --stat` for authoritative hashes and contents. Nothing in this preparation workflow pushes the branch.

## Release decision

The implementation is ready for technical review as an intentionally sliced local series. Official open-source release remains subject to the existing release prerequisites, notably selecting and adding the project license. That governance item does not reduce the functional verification above.
