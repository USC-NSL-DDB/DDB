# DDB TUI breakpoint state and navigation audit

This change follows `e31ee3ad` on `codex/tui-next`. Breakpoint and member rows
retain stable presentation order, member badges report canonical execution
state, and DDB supplies real member hit counts. Keyboard navigation and the
toolbar are updated without changing the public wire contract, dependencies,
configuration, or execution defaults.

## Requirements and implementation

| Requirement | Decision | Verification |
| --- | --- | --- |
| Stop breakpoint rows moving during distributed frame inspection. | Preserve each surviving logical/member row's prior position. Append newcomers, sorting simultaneous new identities deterministically. Keep existing identity-based selection and full-path file grouping. | Reverse snapshot enumerations while dispatching cross-owner frame inspection; assert row and member selection stability. Insert earlier-sorting new identities and assert they append. Real GDB/LLDB reconstructed-caller workflows compare all six logical/member rows before and after selecting another session's frame. |
| Report accurate session/process status. | Use current typed `ExecutionState.stop_reason` and thread states, excluding projected peer causes. Keep exact live-hit attribution separate from historical counters. | Render breakpoint, step, pause, signal, exception, entry, running, and offline states. Check mixed states, missing causes, and peer projections. Real GDB/LLDB tests assert `[stepped]` and the public Step reason. |
| Correct hit counters in the authoritative layer. | Record logical and installed-member observed stops together in DDB, then project one consistent count snapshot. Update resource revisions and state event payloads. | Public API regression, distributed group accounting, retirement, concurrent hits, process-wide stop deduplication, event payload assertions, and real repeated GDB/LLDB hits. |
| Add keyboard navigation consistently. | Ctrl+P moves up and Ctrl+N moves down in all non-Source panels and navigation overlays. Reuse arrow dispatch. Source keeps Ctrl+P for its catalog and supports hjkl plus arrows. Prompt editing is unchanged. | Compare rendered navigation with arrows in all nine non-Source panels. Exercise an overlay while Source has focus, prompt preservation, and Source horizontal/vertical movement. |
| Reduce toolbar verbosity. | Keep action names and button boundaries, put shortcuts in searchable `?` help, and share execution/navigation rows when they fit. Remove the persistent Auto explanation from status. | Complete labels and mouse hitboxes at 140, 100, 80, and 45 columns; Unicode labels, tiny-terminal bounds, capability filtering, and real terminal navigation. |

## Root causes

The backend's direct snapshot enumerates internal maps, while the SDK's state
projection produces a deterministic identity-sorted snapshot. Both represent
the same resource set, but their vector order can differ. The TUI previously
replaced its presentation arrays with either enumeration. Frame inspection and
operation-driven refreshes could therefore move rows. The TUI now preserves
presentation order using its existing arrays rather than adding a persistent
ordering cache. The SDK was not an unordered projection and did not need a
compatibility change.

DDB already counted logical breakpoint stops, but its API projection hard-coded
every `SubBreakpoint.hit_count` to zero. The runtime now records counts by
logical breakpoint, installed member, and session under one lock. Snapshot
projection reads logical and member counts together. A hit event publishes the
updated breakpoint with changed member counts and revisions. The internal
member-count map is omitted from legacy MI JSON serialization.

The backend already provides the typed reasons needed by the TUI. The previous
member rendering only displayed current pauses and could not distinguish step
completion, interruption, and signals. The new badges consume the existing
contract directly. No new endpoint, speculative status API, SDK fallback, or
client-side hit-counter reconstruction was introduced.

## Counter and status meanings

`total hits:N` counts DDB-observed breakpoint stop reports. The logical total
includes the breakpoint's entire lifetime, including retired members. A member
counts its own reports. Conditions and ignore counts can prevent native
encounters from producing a stop report; those encounters are not counted.
The real regression ignores three native encounters, stops at `counter=3`, and
expects one observed logical hit and one member hit. Later effective stops
increase both counts to two and three. A completed step leaves both unchanged.

A process-wide stop counts once, before DDB projects its cause onto peer
threads. Repeated reads, refreshes, steps, and signal stops do not add hits.
Deleting a member does not lower the logical total, so that total can exceed
the sum of currently installed members.

`[1 hit]` and `[N hits]` count current matching stopped threads, not historical
encounters. Other member badges report the session's current execution state.
`[stepped]` means the step completed and the target is stopped. The contract
does not identify an active per-thread stepping action, so the TUI does not
claim that a running thread is specifically stepping. `[mixed]` reports
different thread states or principal stop reasons. `[bp stop]` reports a
breakpoint cause without claiming attribution to this particular installed
member. Missing, unavailable, and disconnected state stays explicit.

## Verification

| Check | Result |
| --- | --- |
| Backend unit tests | 351 passed, 2 ignored |
| API, SDK, generated-schema conformance, targeting, breakpoint synchronization, and execution integration tests | 19 passed |
| TUI unit tests | 361 passed |
| Non-ignored terminal helper tests | 4 passed |
| Complete ignored terminal suite | 36 passed, including five real greeter workflows and GDB/LLDB |
| Focused strengthened reconstructed-caller ordering workflows | 2 passed, GDB and LLDB |
| Unchanged helloworld `.ddb/ddb.yaml` | 1 additional real workflow passed |
| Backend and TUI strict all-target/all-feature Clippy | Passed |
| Formatting and patch whitespace | Passed |

The ordering, public member counter, typed badge, and keyboard regressions were
observed failing before their fixes. Initial verification also found an old
grouping test coupled to incoming array offsets and a new real test requesting
a snapshot without the Execution section. The grouping fixture now selects by
identity; the real test requests the owning thread's execution state explicitly.
No timeout increase, sleep-based fix, or production tracing was retained.

Reproduce from the repository root after building DDB and the real fixtures:

```bash
cargo test --manifest-path ddb/Cargo.toml --locked --offline -p ddb --bin ddb
cargo test --manifest-path ddb/Cargo.toml --locked --offline -p ddb \
  --test api_v2 --test api_v2_client --test api_v2_spec_conformance \
  --test api_v2_targeting --test breakpoint_sync --test execution_control
cargo clippy --manifest-path ddb/Cargo.toml --locked --offline -p ddb \
  --all-targets --all-features --no-deps -- -D warnings
cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --all-targets --all-features -- -D warnings

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock -- --ignored --test-threads=1

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
DDB_GREETER_PROJECT_CONFIG=/absolute/path/to/helloworld/.ddb/ddb.yaml \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock greeter_auto_attach_single_config_managed_terminal_workflow \
  -- --ignored --exact --test-threads=1
```

See the [terminal test prerequisites](../../ddb-tui/README.md#end-to-end-tests).
The project-config check uses
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld/.ddb/ddb.yaml` without
editing it. The user quit their TUI before the real checks. Tests refuse an
occupied discovery port, preserve its prior discovery file, and clean up only
their owned backend, broker, and applications.

## Commit boundaries

All commits use explicit path staging from the clean baseline:

1. `9a894465`, `fix(api): account for installed breakpoint member hits`.
   Authoritative accounting, API projection, backend regressions, and public
   counter semantics. No wire-schema change.
2. `40074be8`, `fix(tui): stabilize breakpoint rows and report typed execution status`.
   Stable ordering, typed member badges, navigation aliases, concise toolbar,
   searchable help, and unit regressions.
3. `test(tui): verify breakpoint state and distributed ordering`.
   Real repeated-hit/step assertions, reconstructed-caller row stability,
   current user documentation, and this audit.

Review the complete change with `git diff e31ee3ad..HEAD -- ddb-tui ddb/core ddb/docs`.
