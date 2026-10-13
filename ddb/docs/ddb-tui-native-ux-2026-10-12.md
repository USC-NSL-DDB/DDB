# DDB native TUI interaction verification

The five interaction fixes below are implemented and verified on `codex/tui-next`
against baseline `a93b9433`. Verification on 2026-10-12 included real GDB, LLDB,
instrumented gRPC greeters, and the existing helloworld project config unchanged.
This work changes the frontend, not DDB's API or configuration contract.

## Requirements and delivered behavior

| Requirement | Behavior | Regression evidence |
| --- | --- | --- |
| Breakpoints from configured local sources must appear in Source. | An untagged local document without a frame-owner hint projects markers for its exact declared debugger path across sessions. It never infers a breakpoint target. Remote, tagged, and frame-bound documents retain owner matching. | `unscoped_local_paths_project_breakpoints_but_never_infer_an_owner`; `local_config_source_marks_and_removes_the_created_breakpoint_without_owner_tags`; real local-tree greeter workflow. |
| Actions must visibly progress. | A local 100 ms spinner runs while mutations or source/frame/variable reads are pending. Labels distinguish queued, admitted, completed, failed, and unconfirmed outcomes. | `pending_actions_have_a_visible_activity_indicator`; `inspection_progress_clears_on_empty_results_errors_and_failed_queue_admission`; existing feedback ordering, timeout, fanout, and disconnect tests. |
| A stop must reveal its actual owner without concurrent focus theft. | The first observed ordinary stop selects its thread, expands topology ancestors, and follows its frame/source. Later concurrent stops retain the first owner's focus while it remains paused. Breakpoint rows and the title count current hits separately from historical counts. | `first_breakpoint_stop_selects_its_topology_and_later_concurrent_stops_do_not_steal_it`; `pause_unfolds_the_owner_and_requests_its_source_even_after_manual_browsing`; real RPC hit checked before manual navigation. |
| Controls need sensible defaults. | `Auto` broadcasts Continue/Pause. Stepping uses the selected paused frame's owning thread, including distributed callers, or the selected paused thread when no frame exists. `c` explicitly overrides controls and cycles back to Auto. | `controls_continue_all_by_default_but_step_the_selected_thread`, including Pause; distributed-frame/boundary and explicit-override regressions; greeter stepping without scope workarounds. |
| Normal UI must avoid internal bookkeeping noise. | Action feedback, hits, startup, and connection labels use developer-facing information. Operation IDs and stop revisions remain available through `--trace-api`; internal identity checks remain unchanged. | `ordinary_action_feedback_does_not_expose_internal_ids`; diagnostic-mode feedback assertions; real GDB/LLDB live-hit view asserts the owner label without IDs/revisions. |

The deterministic interaction regressions are in
[native_ux_interaction_tests.rs](../../ddb-tui/src/native_ux_interaction_tests.rs).
Owner-isolation checks are in
[source_binding_tests.rs](../../ddb-tui/src/source_binding_tests.rs).
Real workflows are in [e2e_mock.rs](../../ddb-tui/tests/e2e_mock.rs).

## Root causes and implementation choices

Local source marker matching previously required a session owner even when an
explicitly mapped local root had none. The fix belongs in `DebuggerBinding`,
which already centralizes both rendering and breakpoint-toggle matching. An
aggregate local view is not ownership. Exact paths remain mandatory; basename
and suffix matches are not accepted. Tagged roots with no matching session do
not become unscoped. Source target discovery still comes from DDB.

Controls previously reused the topology-selected execution target for every
action. `App::control_target` now resolves ordinary controls in one place for
toolbar visibility, keyboard dispatch, and execution prompts. Explicit overrides
retain capability checks and retired-target protection. Browsing sources does
not retarget execution. Evaluation still uses the selected frame's context, and
signals retain their existing single-session picker rather than broadcasting.

Stop handling previously inspected only a stop that matched the already selected
thread. `select_pause_thread` records the first pause owner, reveals its topology,
and enables source following before ordinary inspection. Subsequent concurrent
stops still update state and hit counts. Resuming, backend replacement, or a
successfully queued execution command permits a new focus decision. A command
rejected by the local queue preserves the established owner. Existing generation
and stop-revision checks continue to reject stale inspection and hit replies.

Feedback previously showed state transitions without a dedicated animation tick
and included local tickets, opaque targets, and operation IDs by default. The
existing feedback module now supplies activity and diagnostic views. The 100 ms
timer only advances local rendering while work is active; it sends no requests
and does not change the recovery refresh interval. Empty reads, inspection
errors, queue rejection, disconnect, and last-session retirement clear frame
activity. An admitted command with a lost outcome remains unconfirmed, not
falsely failed or automatically retried.

## Backend evidence and ordering limits

The current public contract already supplies the necessary ownership data in
[resources.proto](../proto/ddb/api/v2/resources.proto):

- `Thread` carries its session, group, state, and top-frame location.
- `ExecutionState` carries its routed target, stop reason, source location, and
  revision.
- `StopReason.thread_id` identifies the thread principally responsible for a
  process-wide stop; `breakpoint_id` identifies its breakpoint.

The worker consumes ordered SDK state updates. It excludes peer projections
whose principal stop thread differs from their routed target. Automatically
paused peers are not counted as breakpoint hits. No new backend field or
compatibility fallback was needed.

"First" means first observed by this frontend, not earliest remote wall-clock
timestamp. Bootstrap or recovery snapshots can contain several existing stops
without their arrival history; ties follow snapshot order. This work does not
claim global chronological ordering across a replay gap.

## Verification results

| Check | Final result |
| --- | --- |
| Frontend unit suite | 349 passed, including 11 added regressions |
| Non-ignored terminal helper tests | 2 passed |
| Complete ignored terminal suite | 36 passed, including all 5 real greeter workflows |
| Existing helloworld `.ddb/ddb.yaml`, unchanged | 1 additional real greeter workflow passed |
| All-target, all-feature Clippy | Passed with warnings denied |
| Rust formatting and patch whitespace | Passed |
| Discovery listener and test-owned applications after verification | Port 10101 free; no remaining TUI, managed DDB, or greeter test process |

The five original focused regressions failed before production changes with
missing local gutter markers, thread-scoped Continue, retained prior-thread
focus, no activity indicator, and exposed operation IDs. They then passed with
the implementation. The initial full-suite runs exposed obsolete assertions for
`◎ thread` and raw hit IDs, plus a greeter test that assumed the session header
remained visible when its thread list scrolled. The final assertions check Auto,
rendered frames, developer-facing ownership, and the selected stopped RPC thread
itself. No readiness timeout was increased.

The real test initially refused an occupied discovery port. After the user quit
their TUI, verification ran without terminating that session or its greeter.
The fixtures preserve/restore the discovery file and clean up only their owned
brokers and applications. No other discovery session should run concurrently.

Reproduce from the repository root, with the sibling DDB and debugger fixtures
built as described in the [README](../../ddb-tui/README.md#end-to-end-tests):

```bash
cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --all-targets --all-features -- -D warnings
cargo fmt --manifest-path ddb-tui/Cargo.toml -- --check
git diff --check

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock -- --ignored --test-threads=1

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
DDB_GREETER_PROJECT_CONFIG=/absolute/path/to/helloworld/.ddb/ddb.yaml \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock greeter_auto_attach_single_config_managed_terminal_workflow \
  -- --ignored --exact --test-threads=1
```

The exact application-local run used
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld/.ddb/ddb.yaml`.
It verified local browsing before attach, a local-tree gutter marker, both
automatic attachments, broadcast Continue, automatic RPC owner/source selection,
an exact `"Hello "` watch, frame-owned Step Over, refresh, and owned-broker shutdown.

## Commit boundaries

The worktree was clean at the baseline. Each commit uses an explicit staging
allowlist; no unrelated changes or generated assets are included.

1. `75dced5c`, `fix(tui): project breakpoints onto unscoped local sources`.
   Only source binding and its owner-isolation tests.
2. `52585f16`, `fix(tui): make execution controls and stop feedback DDB native`.
   Execution/pause policy, feedback and rendering, worker labels, help, and
   deterministic interaction regressions. These share the same App state and
   action dispatch paths.
3. `test(tui): verify native UX with real greeters and document behavior`.
   Strengthened terminal workflows, current user documentation, and this audit.

Review the complete change with `git diff a93b9433..HEAD -- ddb-tui ddb/docs`.
