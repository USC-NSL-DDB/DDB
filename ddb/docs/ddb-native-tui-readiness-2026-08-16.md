# DDB-native TUI gap-closure readiness

Date: 2026-08-16

Status: historical checkpoint, not a completion sign-off for the current TUI work.
The August implementation was recorded as five allowlisted local commits described
below; no remote branch was updated while preparing that checkpoint. Its feature
descriptions, compatibility paths, and test counts must not be read as current.

## Follow-up status, 2026-10-10

Current code inspected: DDB `f2aa85ea`, VS Code adapter `f3c671f`. The full requested
UI/keyboard and adapter-parity goal remains open. The sections after this follow-up
preserve the August checkpoint; see [the TUI guide](../../ddb-tui/README.md) for
current commands and interaction behavior.

Recent auditable changes include guarded frontend logpoints (`37599d3b`), the LLDB
source-mapping command fix (`069424ab`), explicit debugger-session settings
(`c61155f0`), byte-backed debugger output (`ddf7bb62`), and synchronization of
greeter test navigation with rendered discovery (`f2aa85ea`). The latter fixes a
test race: backend discovery could precede the TUI's projection, inserting the
client above an already advancing scan. It does not establish a fix for every
previous source-loading timeout.

### Requirement audit at `51f365fa`

This audit compares the current TUI with adapter `f3c671f`, including
`src/v2/session.mts`, `src/v2/inspection.mts`, `src/DDBViewProvider.ts`,
`src/sessionControls.ts`, `src/sessionTreeItems.ts`, and the focused-frame command
in `src/frontend/extension.ts`. Matching DAP request names alone is insufficient:
the extension itself exposes additional user workflows.

The following evidence is in `ddb-tui/src/main.rs`, `src/ui.rs`, and
`tests/e2e_mock.rs`. Test names identify assertions, not a claim that all possible
interactions are covered. The current unit gate passed 223 tests and two
non-ignored integration helpers at `51f365fa`.

| Requested requirement | Current evidence | Remaining verification or implementation |
|---|---|---|
| 1. Default distributed all-stop | Unit coverage exists, but two current real greeter runs failed the server-stop all-stop assertion | **Not complete. Fix and reverify the reproduced failure described below** |
| 2. Natural source scrolling | `source_vim_keys_navigate_and_paging_tracks_viewport_height`; separate navigation/execution marker render test; real GDB/LLDB workflows assert changed execution line after Next | Re-run terminal workflows after remaining interaction changes |
| 3. Full current adapter feature set | Distributed stacks, source ownership, logical/member breakpoint navigation, paused-hit selection, typed signals, watches, variable/register editing, logpoints, startup commands and source mappings have implementation and focused tests | **Not complete.** See the concrete parity backlog below |
| 4. Expansion and clipped content | Pane border hit tests; horizontal scrolling tests; real memory and breakpoint expansion; wrapped timeline fix `51e4e508`; extension data navigation fix `51f365fa` | Long extension action labels and a single event exceeding the paragraph's 16-bit scroll range remain unverified/incomplete |
| 5. Register scrolling | Independent focus/render tests and `register_keys_scroll_rows_and_values_without_inspection_requests`; real GDB/LLDB register assignment and restoration | Final terminal rerun |
| 6. Optional timeline | Hidden by default; `timeline_is_optional_and_expansion_uses_the_available_width`; independent Output pane tested with timeline hidden | No known missing toggle behavior; oversized-event limitation remains under item 4 |
| 7. Stopped-thread stack/source navigation and inline errors | `selecting_a_stopped_thread_restores_source_following_after_manual_browsing`; `missing_source_is_visible_in_source_pane`; real greeter thread selection and source markers | Keep checking late replies and the intermittent source-loading timeout, not only static rendering |
| 8. Help layout | `help_wraps_scrolls_and_reclamps_after_resize`; modal keyboard/mouse test | Update help alongside every new binding and recheck narrow terminals |
| 9. Current-frame control | `frame_toolbar_control_is_mouse_addressable`; `frame_shortcut_reveals_distributed_owner_without_changing_execution_target` | Preserve source reveal versus execution-jump distinction in subsequent changes |
| 10. UI polish | Explicit loading/unavailable states, pane focus, expansion controls, output separation, bounded output and Unicode viewport tests | Complete details/grouping and clipped action-label work; final rendered interaction review |
| 11. Keyboard polish | Lowercase j/k navigation, uppercase J execution jump, local navigation without backend commands, modal help, independent extension actions/data | Add discoverable bindings for remaining parity features without stealing existing navigation keys |

#### Concrete adapter-parity backlog

Do not remove these items from the goal merely because the TUI offers a more
generic command that can produce a similar backend effect.

1. **Source expression inspection.** Adapter `inspection.mts::evaluate` sends
   `EVALUATION_CONTEXT_HOVER` for hover requests and returns expandable children.
   The TUI currently sends REPL for `e` and WATCH for retained watches, with no
   source-expression hover/inspect interaction. Add a terminal-appropriate,
   explicit inspection action using HOVER, preserving frame ownership and stale
   response rejection. Do not automatically evaluate expressions merely because
   the mouse moves over source text.
2. **Alternative grouping views.** `DDBViewProvider.ts` registers session grouping
   toggling and breakpoint file grouping/flat reset. The TUI currently always
   builds grouped topology and logical/member breakpoint rows. Add view toggles
   that preserve opaque selection identities, execution scope, and mouse mapping.
   Test switching views while sessions and breakpoint members change.
3. **Details and copying.** The adapter exposes group details plus session fields
   and a copy-details command. The TUI needs an accessible, scrollable details view
   and an explicit copy/export interaction for the available canonical fields.
   Do not fabricate legacy numeric IDs or equate them with opaque API IDs. Avoid
   silently writing terminal clipboard escape sequences on selection or hover.
4. **Confirmed session termination.** `sessionControls.ts` captures the selected
   session before confirmation and sends SIGKILL only after acceptance. The TUI's
   signal picker is not an equivalent confirmed kill workflow. Add an explicit
   session action, freeze its target while confirming, and reject stale/removed
   targets. Test cancellation and focus changes without terminating real user
   processes.
5. **Variable-to-memory navigation.** Adapter `inspection.mts` preserves
   `variable.address` and evaluation addresses as memory references. The TUI now
   preserves both fields, including expanded children, and `M` in Variables reads
   the selected address through its owning distributed frame. Keyboard-level
   tests cover address preservation, owner targeting, watch refresh invalidation,
   and rejection of absent addresses, boundary frames, and running threads.
   Backend decoders and projections preserve address metadata, and both native
   backends now supply storage addresses. Real GDB/LLDB scope tests compare memory
   reached through these addresses with fixture expressions. The real
   distributed-caller case and additional unavailable/synthetic-value coverage
   listed below remain open.
6. **Editor-hosted watch behavior.** Review watch editing/persistence against the
   adapter plus its VS Code host before declaring inspection parity. Existing
   add/remove, refresh and expansion prove only those narrower capabilities.
   Record missing behavior and implement the terminal equivalent rather than
   silently excluding it.

These findings change the next implementation work. The full goal is not ready
for sign-off even if the current terminal suite is green.

#### Current terminal verification and all-stop failure

At `51f365fa`, all 22 non-greeter ignored terminal workflows passed in 137.17
seconds, including real GDB and LLDB inspection/stepping, register edits and
restoration, logpoints, startup settings, source explorers, and lifecycle tests.
These do not establish distributed all-stop correctness.

Two subsequent real auto-attached greeter runs failed after 44.54 and 47.25
seconds. The second run's backtrace identifies `tests/e2e_mock.rs:175`, the wait
after the second broadcast Continue. Its final snapshot shows the server stopped
at `greeter_server.cc:59` in `GreeterServiceImpl::SayHello` while the client session
and its threads remain Running. This is not merely a missing source marker.
The first client-stop assertion had already succeeded.

The second snapshot's Execute operations include the two broadcast requests and
multiple completed server-session requests afterward, but no client-session
request. This narrows investigation toward event/control reconciliation; it does
not prove why the client interrupt was absent. The full second-run test log is
retained locally at `/tmp/ddb-greeter-audit.EBiv13/run.log`, not checked into Git.
Earlier successful runs below are historical evidence, not grounds to dismiss
these current failures.

Prioritize this failure before the feature backlog. Capture the delivered
canonical stop/running events, snapshot cursor, control epoch, all-stop cause,
and queued peer target around the second Continue. Distinguish a stale event,
suppressed reconciliation, and delayed worker dispatch with a regression that
reproduces the observed event order. Do not increase the test timeout or weaken
its all-thread assertion to get a green result. No production all-stop changes
were made during this audit.

Follow-up tracing at `2d5e4b3d` did not reach the second stop: the unchanged
workflow repeatedly failed its initial source-marker wait with the source pane
still displaying Loading source. File-only probes showed initial stop delivery
to the UI within roughly 60–143 ms. Hiding the optional timeline in a temporary
comparison did not resolve the source timeout. In the restored original workflow,
the first 300 terminal draws consumed 1.39 seconds cumulatively; that run failed
after 19.33 seconds. This does not support attributing the whole source timeout
to timeline rendering, and it does not diagnose the later all-stop failure.

The measured original run is retained locally under
`/tmp/ddb-allstop-trace.Uwo5pr/`; file-only initial-event traces also exist under
`/tmp/ddb-allstop-trace.Z9Q3Xn/` and `/tmp/ddb-allstop-trace.YJAbbf/`. Early attempts
that printed into the PTY disturbed screen capture and are not valid control
evidence. Temporary local-source experiments also changed selection/setup and
are not substitutes for the original workflow. All probes and test experiments
were removed, including the diagnostic environment hook. No timeouts were
increased. Next trace source request generation, enqueue/start/completion, and
the backend resolver/catalog command to distinguish source work from message
delivery before returning to the later stop-event trace.

### Source-loading investigation

#### Latest request-flow trace and event-scope correction

Following `31afc6c8`, file-only request probes showed source generation 1 starting
in the worker immediately, with no replacement request cancelling it. In a run
that reached the later all-stop failure, ResolveSource took 8.42 seconds for the
server and 9.89 seconds for the client; ReadSource completed in milliseconds and
the UI consumed each result within about 60 ms. That run's logs are retained at
`/tmp/ddb-source-flow.552jx9/`.

A backend-instrumented failure in `/tmp/ddb-source-flow.FXw7Ou/` acquired the
per-session source gate immediately and then remained inside the command
executor's `-file-list-exec-source-files` call until the test's source deadline.
This identifies the command-execution boundary, not whether the delay is inside
GDB or DDB's command dispatch/completion machinery. No catalog scope, timeout,
or source ownership semantics were changed.

The investigation also found and corrected an independently reproducible event
projection defect. Session/group running summaries became `Running(None)`, which
the UI interpreted as every thread resuming. That could invalidate stopped peers
and erase the all-stop cause. Running activity now requires an explicit opaque
thread ID. Aggregate summaries continue to update the preceding canonical
snapshot but do not manufacture thread transitions. The new
`aggregate_running_states_do_not_impersonate_global_thread_resumes` test failed
before the fix and passed afterward; it also checks a real thread target still
produces running activity.

The first real greeter attempt after this correction failed while the terminal
helper searched for the client thread, before reaching the second breakpoint.
Its trace, `/tmp/ddb-source-flow.f5vsaB/`, shows the initial client stop queueing
and dispatching a server-session pause, but does not prove the later all-stop
failure fixed. Source discovery in that run returned 3,464 paths in about 6.6
seconds. The full goal remains open. All temporary probes were removed.

After cleanup, the event-scope correction passed 224 unit tests, two non-ignored
test helpers, all 22 non-greeter terminal workflows in 135.05 seconds, formatting,
and Clippy with warnings denied. The ordinary backend binary was rebuilt without
the temporary diagnostic hook. The original greeter test and its deadlines are
unchanged.

#### GDB native storage-address verification

GDB now uses DDB-native variable-create and child-list wrappers. Standard MI
performs the requested value operation once. Address enrichment parses only
bounded native lvalue paths and uses symbol/value APIs for fields, pointers, and
array elements; it never calls a second general expression evaluator. Function
calls, assignments, casts, arithmetic, and unsupported synthetic paths receive
no inferred address. Nonlocal values retain native scope-symbol identity.

Child reads explicitly include both the owning thread and frame. The initial
real GDB run exposed that GDB rejects `--frame` without `--thread`; the corrected
command builder has a regression asserting the pair and a nonzero frame level.
LLDB and the mock backend implement the same internal command names. Existing
standard MI entry points remain available for stdin clients.

Verification on 2026-10-10:

- Both real GDB/LLDB scope contracts passed with native address/memory comparisons
  for locals, nonlocals, expanded fields, and evaluated watches.
- Real TUI workflows passed for GDB in 26.67 seconds and LLDB in 21.23 seconds.
  Both use `M` on a watch and verify the complete native-endian `31337_u64` value.
  The mock terminal workflow passed in 4.91 seconds.
- Four GDB resolver tests and 13 LLDB bridge tests passed. GDB tests assert that
  unsupported expressions trigger no native value lookup, a function-call watch
  invokes standard variable creation only once, and no-values child requests do
  not query storage paths.
- 336 backend tests passed serially with one intentional ignored test. One
  earlier parallel run failed the telemetry test
  `operation_logs_exclude_error_and_debugger_payload_text` because its captured
  log lacked `op_safe_reference`. It passed in isolation and serially. This is
  retained as an unresolved parallel-test failure, not reported as a clean
  parallel backend gate.
- 230 TUI unit tests and two integration helpers passed, as did strict backend
  binary Clippy, strict TUI all-target Clippy, formatting of touched Rust files,
  and diff whitespace checks.

Real distributed-caller memory navigation and the broader adapter-parity audit
remain open. These checks do not prove storage support for every language-level
expression or synthetic pretty-printer value.

#### LLDB native storage-address verification

The LLDB bridge now emits storage addresses for requested native values, and the
backend validates and preserves them through locals/scopes, variable objects,
children, and object-backed evaluation. It does not parse display values or
evaluate an extra address expression. Invalid/unavailable storage is omitted;
metadata-only scope lookahead and `--no-values` stack queries do not fetch it.

Verified on 2026-10-10:

- 335 backend unit tests passed, with one intentional ignored test.
- 13 bridge tests passed, including storage versus displayed pointer value,
  invalid/unavailable values, scope lookahead, and no-values requests.
- The real LLDB scope contract passed with address-based memory reads checked
  against known fixture expressions for locals, globals/statics, expanded fields,
  and watch evaluations. The GDB scope regression also passed with addresses
  still absent.
- The real LLDB TUI workflow passed in 18.08 seconds. It selects `Watch: counter`,
  presses `M`, expands Memory, and verifies all eight native-endian bytes of
  `31337_u64`. It then scrolls the reduced Variables viewport before continuing
  child expansion. The first extended run passed the memory check but exposed
  the test's assumption that children were immediately visible; the final test
  explicitly navigates to them.
- 230 TUI unit tests and two integration helpers passed. Strict backend binary
  Clippy, strict TUI all-target Clippy, touched-file formatting, and diff whitespace
  checks passed.

The initial API memory comparison used the fixture's insufficient read-only
token and received 403; the corrected test uses its memory-authorized token.
Production authorization was not changed. GDB native addresses, real distributed
caller memory navigation, and the remaining parity requirements are still open.

#### Native value-address implementation boundary after `983dac85`

At `983dac85`, the frontend action was implemented but native address support was
not. The following trace records the implementation boundary; native metadata and
backend propagation have since been implemented for both debuggers:

- `application/debugger_reads.rs` drops address metadata at all three decode
  boundaries: stack/scope rows, created variable objects, and expanded children.
  Add an optional storage-address field to both decoded value types and preserve
  it through `projection.rs`, requested-page metadata enrichment in `service.rs`,
  and object-backed evaluation completion in `mutations.rs`.
- `assets/lldb_ext/ddb_lldb_bridge.py` already holds native `SBValue` objects for
  those paths. Obtain storage addresses from native values only where the value
  is already requested. Omit invalid addresses, register-only values, and
  unavailable values. Do not confuse a pointer's storage with its pointee.
- `assets/gdb_ext/runtime-gdb.py` holds native symbols for nonlocal scope reads,
  but ordinary locals and variable-object operations currently use standard MI.
  GDB needs a native-value address path as well; merely extending the decoder or
  the LLDB bridge does not complete this work. Preserve symbol/frame identity
  rather than resolving a potentially ambiguous display label. Expanded
  pretty-printer children may have no native storage, which must remain an
  explicit absent address rather than a guessed expression.
- Nonlocal pagination deliberately requests one metadata-only lookahead record.
  Both bridge implementations skip fetching its value. Address extraction must
  stay inside the requested-value branch, never symbol enumeration, sorting,
  continuation lookahead, or a `--no-values` operation.
- Keep existing stop-revision validation before and after native reads and
  variable-object lifetime bounds. Do not retain otherwise disposable leaf
  evaluation objects solely to preserve an address string. A resume invalidates
  the associated frontend row and outstanding memory reply as it does today.

Use debugger-native address metadata, not display-value parsing or automatic
evaluation of `&(expression)`. GDB documents `Value.address` as an optional
address for addressable values, and LLDB provides storage-address access on
`SBValue`: [GDB value API](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Values-From-Inferior.html),
[LLDB value API](https://lldb.llvm.org/python_api/lldb.SBValue.html).

Required verification before closing this parity item: decoder tests for absent,
valid, and malformed metadata; native bridge tests proving no lookup for the
lookahead record; real GDB and LLDB locals, globals/statics, expanded children,
and watch results; unavailable/register-only/synthetic values; pointer storage
versus pointee distinction; shadowed names; and stale-stop rejection. Finally,
drive `M` through the real TUI and compare the returned bytes with a known fixture
value, including a distributed caller frame. Existing manual `$sp` reads are
regression coverage, not evidence that selected-value navigation works.

#### Atomic, ordered snapshot delivery after `a83e2e35`

Refresh requests and the SDK event watcher previously delivered snapshots and
threads as separate messages. Their messages could interleave, and an older
stream projection could overwrite a newer refresh. The TUI now installs topology
and threads in one UI message, rejects decreasing state revisions or event cursor
sequences within an instance, and rejects late refreshes from a previous backend
instance. A newly observed canonical stop in an accepted snapshot also triggers
all-stop reconciliation, without depending on a separate stop event arriving.

Regression tests exercise the actual UI message handler: atomic thread delivery,
older topology at the same state revision but a lower cursor sequence, snapshot
stop reconciliation and duplicate suppression, stale resume rejection, and a
delayed refresh after a backend-instance change. The atomic delivery and stale
topology assertions failed before their fixes; the equal-revision cursor case
also failed with revision-only ordering.

Verification of this patch on 2026-10-10:

- 228 unit tests and 2 integration helpers passed.
- All 22 non-greeter ignored terminal workflows passed serially in 97.36 seconds,
  including real GDB/LLDB, logpoints, debugger settings, and managed lifecycle.
- The unchanged real greeter auto-attach workflow passed twice consecutively in
  48.46 and 55.86 seconds. Both runs checked client and server breakpoint stops,
  all-stop, source markers, and breakpoint-to-source navigation. Logs were retained
  locally at `/tmp/ddb-greeter-state-order.r60kgP/run-1.log` and `run-2.log`.
- TUI formatting, strict all-target Clippy, and `git diff --check` passed.

These passes do not establish full adapter parity or eliminate every previously
observed source-loading timeout. The requirement audit and remaining parity work
above still apply. No test deadline or greeter configuration was changed.

#### Earlier measurements at `f2aa85ea`

Four instrumented runs of `greeter_auto_attach_all_stop_terminal_workflow` passed
on `f2aa85ea`. Temporary timers covered `ResolveSource`, the per-session
catalog gate/provider, descriptor construction, and `ReadSource`. Observations
for this instrumented gRPC helloworld checkout and debug backend build:

| Stage | Observed elapsed time |
|---|---|
| Cold debugger catalog discovery | 4.4 to 5.6 seconds |
| Catalog gate acquisition | Tens of microseconds |
| Cached source resolution | About 2 milliseconds |
| Reading source contents | Under 7 milliseconds |

A separate native GDB MI process took 0.32 seconds to load the greeter client
executable and 3.15 seconds to load it and list its source files. That process did
not attach to an inferior, so its timing is not a direct estimate of DDB overhead.
It does show that native source enumeration itself is not instantaneous.

In the measured client lookups, the server catalog was already cached. Therefore
the serial session loop did not explain the timeout in these runs. Neither did
filesystem reads. The previously intermittent timeout was not reproduced after
the navigation-test synchronization change; these successful runs do not prove
that the synchronization change fixed the source-loading problem. No production
lookup scope, ambiguity handling, or test timeout was changed during measurement.
All temporary timing probes were removed afterward.

Reproduce the real workflow from the repository root, with the existing discovery
port free and no other discovery session running:

```sh
DDB_GREETER_WORKSPACE=/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld \
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  greeter_auto_attach_all_stop_terminal_workflow -- --ignored --test-threads=1
```

The remaining completion audit must cover all eleven requested requirements and
the current adapter implementation, not merely this older report's feature list.
If the source timeout recurs, correlate the TUI request enqueue time with backend
resolution start and catalog completion before changing execution or resolution
semantics. Retain the native GDB measurement as a separate baseline.

## August checkpoint

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
