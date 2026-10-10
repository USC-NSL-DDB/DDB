# DDB-native TUI gap-closure readiness

Date: 2026-08-16

Status: historical checkpoint, not a completion sign-off for the current TUI work.
The August implementation was recorded as five allowlisted local commits described
below; no remote branch was updated while preparing that checkpoint. Its feature
descriptions, compatibility paths, and test counts must not be read as current.

## Follow-up status, 2026-10-10

Initial follow-up checkpoint: DDB `f2aa85ea`, VS Code adapter `f3c671f`. The full requested
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

### Active adapter configuration audit, 2026-10-10

The audit inspected `CanonicalLaunchArguments`, `launchRequest`,
`configurationDoneRequest`, and `connection.mts::managedArguments` at adapter
`f3c671f`, rather than treating every inherited package.json property as a working
feature. Deprecated target/arguments/terminal/SSH settings are not implemented by
the canonical adapter and are not a reason to add a legacy transport to the TUI.

| Active adapter option | TUI mapping or remaining gap |
|---|---|
| ddbpath, configFilePath, apiEndpoint, apiToken | `--ddb-path`, positional YAML/`--config`, `connect --api`, and `--token`/`DDB_API_TOKEN`; managed lifecycle and credential isolation have PTY tests. |
| cwd, env | Shell working directory and environment inheritance provide startup control, including removing variables with `env -u`; this does not require a second configuration format. |
| valuesFormatting | `--values-formatting pretty-printers/raw/disabled`; raw corresponds to the adapter's parseText mode. |
| pathSubstitutions, autorun | Explicit `--debugger-settings` source mappings and backend-specific native commands, ordered and tested on GDB/LLDB. |
| stopAtEntry boolean or function | `entry_function: main` or another symbol in debugger settings uses typed temporary breakpoints for each ready/stopped native session, including later discovery, without automatic continuation. Native `launch --stop-at-entry` remains a separate process-start policy. |
| showDevDebugOutput | `--backend-log` captures managed backend stdout/stderr explicitly; inferior/debugger output remains in the Output pane. |
| distributedStack=false | **Open:** the TUI always requests distributed frames. Keep distributed stacks as the default, but provide an explicit local-only choice if completing full configuration parity. |
| printCalls | **Open:** there is no equivalent opt-in sanitized client request trace. The optional timeline is execution telemetry, not HTTP method/path/status diagnostics. |
| debugger_args | **Open:** the managed TUI does not forward additional backend arguments. Any implementation must reject managed endpoint/authentication/report overrides rather than weakening ownership guarantees. |
| pairedBreakpointRequests | Adapter/extension coordination detail; TUI directly creates typed logical breakpoints with explicit group/session/multiple targets, so no paired DAP request protocol is needed. |

These concrete gaps replace the vague remaining configuration sign-off. The
entry-function addition alone does not establish complete adapter parity.

Entry setup verification: the real settings workflow passed with GDB and LLDB
in 6.56 seconds. It checks that the breakpoint is temporary and verified
before native startup commands finish, that setup has not resumed the process,
and that Continue reaches the configured function with a source marker and
`counter = 0`. The first version of this test incorrectly waited for hidden
timeline text despite reaching the function; it now checks canonical stopped
function state and visible source/value output without changing its timeout.
The contract test asserts exact session targeting, mapping/entry/command order,
feature gating, and stopping after mapping or entry failure. The lifecycle test
checks one-time setup across refresh, a later discovered LLDB session, removal,
and backend-instance replacement. Validation rejects blank, oversized and
control-containing symbols while retaining C++ function expressions with spaces.
The module-design guidance kept this work inside the existing ordered setup
module, avoiding a second startup lifecycle in the input loop.

The implementation gate passed 273 unit tests and two integration helpers. The
added late-discovery lifecycle assertion also passed in a focused rerun. All
26 non-greeter PTY workflows passed in 79.54 seconds; strict all-target Clippy,
formatting and diff checks passed after the final test change. This entry-setup
change did not rerun greeter; its preceding 33.23-second pass is recorded below.

### Current requirement audit at `fe1eb296`, 2026-10-10

The worktree was clean for this audit. Adapter HEAD remains `f3c671f`. The
following replaces the older `51f365fa` matrix as the current evidence summary;
that older matrix remains below to preserve the history of reproduced failures.
This is not yet a full parity sign-off.

| Requirement | Inspected evidence and current result |
|---|---|
| 1. Default all-stop with greeter | Real auto-attach PTY workflow passed again in 33.23 seconds on this commit. It creates both breakpoints through TUI input, continues to the client and then server breakpoint, asserts all discovered threads stopped at both stages, checks the server's canonical breakpoint stop, and opens its source through the paused-hit dialog. |
| 2. Natural source scrolling | `SourceBrowser::fit_viewport` keeps the viewport stationary until the cursor crosses a visible edge and reclamps on resize. Keyboard assertions check j/k and page size changes without backend requests. Execution and navigation markers have separate rendering assertions. These tests passed in the 272-test gate. |
| 3. Adapter feature parity | Rechecked contributed commands in adapter `package.json`, DAP capabilities in `src/v2/session.mts`, and its custom request dispatch. The previously listed implementation backlog has changes and tests recorded below. A final command/configuration-by-command/configuration mapping is still required before claiming full parity. |
| 4. Expand panes and reach clipped content | Mouse expansion preserves focus and sends no backend command; help blocks underlying clicks. Expanded Variables excludes Registers/Memory. Render tests cover horizontal inspection, memory rows, long topology labels, oversized timeline events and extension cells. Extension paging now excludes the action bar. |
| 5. Register scrolling | Inspected keyboard/mouse assertions advance rows and horizontal columns independently, reset the column with Home, and dispatch no inspection requests. Rendering tests cover focus and resize reclamping. Real GDB/LLDB terminal workflows passed in the 26-test run. |
| 6. Optional timeline | Hidden timeline is skipped during focus cycling. The t toggle shows/focuses it, then hides it and restores Source focus. Output remains accessible independently. Expansion and oversized-event rendering tests passed. |
| 7. Stopped-thread source navigation and errors | Explicit stopped-thread selection restores source following and requests inspection. Missing-source rendering includes the backend reason and F8 retry hint. Greeter exercises actual mouse thread selection and source loading, followed by paused-hit source navigation. Late-response/ownership cases retain their focused tests. |
| 8. Help layout | Inspected 45x14 wrapping/scrolling assertions, bottom-page visibility, 200x100 reclamping, and 1x1/10x3/20x8 render checks. Modal keyboard/mouse behavior is tested. Help includes the latest extension navigation binding. |
| 9. Current-frame toolbar action | Rendered Frame F8 hit rectangle resolves to RevealFrame. F8 uses the selected distributed frame owner's source target without changing execution scope or inspection generation; missing locations and boundary frames report errors. |
| 10. UI polish | Loading/error states, distinct execution/navigation/breakpoint markers, independent inspection panes, explicit expansion controls, frozen topology details, and Unicode clipping have current render/interaction coverage. Subjective visual quality is not established merely by green tests; final parity review must also check discoverability against the adapter. |
| 11. Keyboard polish | Local navigation is tested not to dispatch debugger commands. Modal help/details/confirmation block underlying actions. j/k source navigation is distinct from J execution jump; F8 reveals source; z/Esc expands/restores; extension Home/End and paging have explicit tests. Final parity review must reconcile the remaining adapter command/configuration inventory with documented bindings. |

Current aggregate verification is 272 unit tests, two integration helpers,
26 non-greeter terminal workflows, and this separate real greeter pass. Strict
Clippy, formatting and diff checks passed for the TUI. This does not claim a
clean parallel backend unit gate: the previously observed telemetry test failure
is separate from these results. No timeouts were increased for this audit and
the greeter test verifies that the shared service-discovery config is unchanged.

### Extension data navigation verification, 2026-10-10

The extension renderer no longer passes its vertical offset through Paragraph's
16-bit scroll API. A shared grapheme-aware wrapper caches styled visual rows and
renders only the visible slice. Identical projected snapshots retain the cache;
changed data invalidates it, and width changes reflow it. Timeline uses the same
wrapping function without changing its retention policy.

The regression with one large Unicode cell failed to display its final `TAIL`
before this fix. It now reaches both ends beyond 65,535 visual rows, preserves
wide and combining characters, reflows on resize, and clears stale content.
Additional tests cover 70,000 logical rows, styles, cache reuse and invalidation,
empty content, and terminal control sanitization.

Home/End now selects the first/last extension data page. Shift+Home still resets
only the action label. A separate regression exposed Page Down counting 11 rows
when only 10 data rows were visible under the action bar. Paging now uses the
actual rendered data rectangle, excluding the action bar and borders.

Verification on this change: 272 unit tests and two integration helpers passed;
strict all-target Clippy, formatting and diff checks passed. All 26 non-greeter
terminal workflows passed serially in 76.62 seconds, including real GDB/LLDB
distributed caller inspection. The real greeter was not rerun for this rendering
change; its preceding evidence is recorded below. Adapter HEAD was rechecked as
`f3c671f`. The full requirement-by-requirement completion audit remains open.

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
   The TUI now provides `E` and the clickable source-header `Inspect [E]` action.
   It captures the selected stopped frame before prompting, sends HOVER, and
   shows one expandable `Inspect:` result in Variables. Frame changes clear it;
   it is not retained or automatically refreshed as a watch. Keyboard/mouse and
   worker contract tests cover ownership and stale replies. Mouse movement alone
   never evaluates code. See the verification section below for real workflows.
2. **Alternative grouping views.** `DDBViewProvider.ts` registers session grouping
   toggling and breakpoint file grouping/flat reset. The TUI now offers grouped
   and flat topology through `v` and a clickable header button. It preserves
   opaque session/thread selection and execution scope across switches and
   discovery updates. Breakpoints now also offer file grouping and flat reset
   through focused-pane `v` and header controls. Full paths define file groups;
   non-source locations have a separate group. Group headers are non-actionable,
   and member selection survives snapshot replacement by opaque member ID.
3. **Details and copying.** The adapter exposes group details plus session fields
   and a copy-details command. The TUI now has a scrollable frozen details view
   through `i` in Topology or right-clicking a row. `e` or Export prompts for a
   local destination and writes the captured canonical fields only after Enter.
   Existing paths are never overwritten, and no clipboard escapes are emitted.
   Do not fabricate legacy numeric IDs or equate them with opaque API IDs. Avoid
   silently writing terminal clipboard escape sequences on selection or hover.
4. **Confirmed session termination.** `sessionControls.ts` captures the selected
   session before confirmation and sends SIGKILL only after acceptance. The TUI
   now offers `K` and a topology `Kill [K]` button. The dialog freezes one session,
   defaults Enter/Esc to cancellation, and requires `y` or a left-click on
   Terminate. Both confirmation and the worker reject stale/removed targets.
   See the verification section below; no real user process was killed in tests.
5. **Variable-to-memory navigation.** Adapter `inspection.mts` preserves
   `variable.address` and evaluation addresses as memory references. The TUI now
   preserves both fields, including expanded children, and `M` in Variables reads
   the selected address through its owning distributed frame. Keyboard-level
   tests cover address preservation, owner targeting, watch refresh invalidation,
   and rejection of absent addresses, boundary frames, and running threads.
   Backend decoders and projections preserve address metadata, and both native
   backends now supply storage addresses. Real GDB/LLDB scope tests compare memory
   reached through these addresses with fixture expressions. The public API's
   real distributed-caller path is now verified for both backends, including
   non-top-frame child expansion and stale-frame rejection. The real TUI workflow
   now selects a cross-session, non-top caller frame, expands its marker field,
   and verifies the exact bytes reached with `M` on GDB and LLDB. Literal watches
   and failed evaluations reject memory navigation without changing pane focus.
   See the distributed TUI verification section for setup and failure history.
6. **Editor-hosted watch behavior.** Watch editing is now available with `W` or
   the Variables `Edit [W]` button, including editing from a watch child. The
   existing watch identity is retained; old evaluation replies are rejected;
   only the changed expression is evaluated. Explicit `--watch-file PATH`
   persistence now restores and autosaves expressions across restarts. It never
   restores debugger handles or old values, and does not discover/evaluate watch
   files implicitly from an arbitrary checkout.

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

### Distributed TUI inspection verification, 2026-10-10

`tests/support/real_distributed_memory.rs` runs two real fixture processes for
each debugger. Setup uses only the v2 SDK for frame/register reads and execution
control. It captures the root caller's register context, hands it to the callee,
resumes the root and interrupts it at a different PC. The TUI selects the leaf
thread and obtains a distributed stack. Assertions require leaf-first ordering,
a distributed boundary, the root's saved PC, distinct owning threads, and a root
caller frame whose local level is greater than zero. A stale local stack alone
cannot satisfy these checks.

The terminal workflow expands Stack, clicks `root_stop`, restores the layout,
expands `memory_marker.value`, and opens its storage with `M`. Expanded Memory
must show `9501_u64.to_ne_bytes()`, not merely some bytes. It then evaluates a
literal and an undefined symbol. For each, `M` must report the missing address and
leave focus in Variables, verified by immediately expanding that pane. CI now
builds the existing `real_dbt` fixture with full debug information and no
optimization; no new runtime dependency or compatibility API was introduced.

This workflow exposed two independently reproduced UI defects:

- `878c73b8`: expanded Variables still used the shared three-pane inspection
  split. A focused rendering test failed before the fix. Expanded Variables now
  occupies the full pane and excludes register/memory mouse targets, retaining
  their data for Restore. Wide and short-terminal layouts are covered.
- `0f800d31`: routine snapshots replaced an action error with session counts,
  sometimes between the `M` input and the next expanded frame. The captured PTY
  output showed the error followed immediately by the count summary. A model
  regression reproduced the overwrite. A still-displayed action error now survives
  routine snapshots; another action/result can replace it, and a recovered
  connection error still clears on a successful snapshot.

The focused GDB/LLDB workflows passed together in 7.85 seconds after both fixes.
The unit gate passed 267 tests and two integration helpers, and strict Clippy
passed. All 26 non-greeter terminal workflows passed on the final code in 76.90
seconds. The instrumented greeter auto-attach all-stop workflow also passed on
the final code in 34.52 seconds, following an earlier 31.50-second pass before the
status fix. No test timeout was increased and no probes remain. The fixture tests
use real debuggers and reconstructed caller contexts; the separate greeter test
supplies evidence for the actual gRPC auto-attach/all-stop workflow. The final full-goal audit
remains open. The extension renderer's then-outstanding 16-bit offset limitation
is resolved by the extension data navigation verification recorded above.

### Explicit topology export verification, 2026-10-10

The details dialog now offers `e` and a clickable Export action. Destination entry
captures the already displayed document, not whichever session happens to be
selected when Enter is pressed. Esc and empty Enter cancel without a filesystem
write. An explicit export creates a new local JSON file using buffered writes to
a same-directory temporary file, file sync and no-clobber persistence. Unix files
are owner-only and the parent directory is synced. Existing files, directories,
symlinks and dangling symlinks are not overwritten. Two bounded background export
tasks keep file writes off the UI and debugger-command execution path. There are
no clipboard writes and no debugger operations required for the export.

Export path entry exposed two prompt-rendering defects: compact terminals showed
only status while input was active, and long inputs could hide the cursor. Shared
prompt rendering now shows the active prompt in compact layouts and scrolls long
input to keep the cursor visible. Rendered tests cover long Unicode paths at
heights 8, 18 and 30, including moving back to the start.

The first new terminal test used the source function name `mock_session` as a
session label and failed before reaching export. Its captured screen showed the
actual session label, and the selector was changed to the canonical snapshot's
display name. The exact test then passed, exercising cancellation, a saved JSON
document with matching instance/session IDs, and a second export to the same path
that reports an error without changing the file. No timeout was increased and no
diagnostic instrumentation remains. The adapter revision is still `f3c671f`.

The 265 unit tests and two integration helpers pass, including an offline worker
test that observes both export success and no-overwrite failure. Strict Clippy,
formatting and diff checks pass. All 24 non-greeter terminal workflows passed in
67.93 seconds, including real GDB/LLDB interaction, watch persistence, source
navigation, lifecycle checks and the new export workflow. The real greeter
all-stop workflow was not rerun for this local
export change. Distributed-memory TUI composition and the full-goal audit remain
open; this is not a completion sign-off.

### Topology details view verification, 2026-10-10

The details dialog captures canonical v2 group/session resources, backend instance
and state revision. Thread rows resolve to their owning session without assuming
that thread and session ID namespaces are disjoint. Ungrouped and unresolved-group
rows retain their actual membership and do not fabricate legacy hashes or IDs.
The captured document remains unchanged while discovery updates arrive.

Opening and scrolling the dialog does not change execution scope or issue debugger
requests. Keyboard and right-click input are modal; arrows/hjkl, page keys, Home/End,
and mouse wheels navigate the document. Width is cached when captured. Rendering
uses visible line slices and grapheme-safe horizontal clipping, not a u16 paragraph
scroll offset. Tests reach row 69,999 and the end of a long Unicode line, then resize
between narrow and normal layouts.

Verification: 260 TUI unit tests and two integration helpers passed; strict Clippy,
formatting and diff checks passed. The mock terminal workflow is checked separately
from the rendered details tests. No real greeter or real-backend workflow was rerun
for this read-only view. Export, distributed-memory TUI composition and the final
full-goal audit remain open.

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

#### Watch-file persistence verification

`--watch-file PATH` is available in managed config, launch, attach, and connect
modes. The file is an explicitly trusted, versioned JSON expression list. Startup
validates it and acquires its exclusive sidecar lock before launching a managed
backend. Without this flag, watches remain process-local and no watch file is
read or written. A missing file starts an empty list; its parent must exist.

The persistence module contains bounded parsing, schema/version checks, locking,
external-change detection, and atomic replacement. It saves only expressions,
never values, opaque handles, memory addresses, session IDs, or API credentials.
Expressions themselves may contain sensitive data or have debugger side effects;
the guide documents that trust requirement and owner-only Unix file permissions.

Expression add/edit/remove revisions drive a serial background saver. Evaluation
updates and cancelled/unchanged edits do not rewrite the file. Rapid changes
coalesce, and normal shutdown flushes the latest pending update. Save failures
are observed by the UI without blocking on its backend message channel, and an
unsaved latest update causes shutdown to report failure. Terminal draw errors now
also take the ordinary worker and persistence cleanup path.

An OS advisory lock prevents two cooperating TUIs from editing the same file.
The `.lock` sidecar remains after exit, but its OS lock is released automatically.
Observed external edits are rejected before replacement. This does not claim
transactional exclusion of unrelated editors that ignore the advisory lock.
Watch/lock symlinks are rejected on Unix; invalid/future/oversized files are not
rewritten. Limits are 1 MiB, 64 expressions, and 4096 bytes per single-line entry.

The terminal regression adds and edits a watch, observes autosave before exit,
restarts the frontend using the same file, observes the restored watch, deletes
it, and verifies the persisted empty list. The focused run passed in 1.22 seconds.
Unit tests cover round trips, permissions, exclusive locking/release, invalid
schemas, external edits, symlinks, final-update flushing, and failure reporting.

A parallel unit run exposed an intermittent release failure before the final
gate. A deterministic duplicate-descriptor regression reproduced the same lock
lifetime: closing the owner does not release an advisory lock while a forked
child still holds the shared open file description before exec. The lock guard
now explicitly unlocks on drop, including startup-validation failures. The
regression failed before that fix; acquisition errors now retain their original
OS cause rather than classifying every error as contention.

Final verification on 2026-10-10: the deterministic inherited-descriptor
regression and all 256 unit tests passed, along with two integration helpers.
All 23 non-greeter terminal workflows passed serially in 66.78 seconds, including
real GDB/LLDB, managed lifecycle/credential checks, and watch-file restart coverage.
Strict all-target TUI Clippy, formatting, and diff whitespace checks passed.
The real greeter workflow was not rerun for this change. Details/export and the
remaining distributed-memory TUI verification still prevent full-goal sign-off.

#### Retained watch editing verification

`W` in Variables or its `Edit [W]` border button opens a prefilled expression
prompt for the selected watch or a child row's owning watch. The prompt captures
the watch ID, not the current row index. Esc cancels; empty input is rejected;
unchanged input does not trigger another evaluation. Saving replaces only that
watch's value/child tree and increments its revision before any evaluation, so
late values and child responses from the old expression cannot restore it.

Watch identity and evaluation setup remain in the watches module. The edited
watch uses the current selected stopped frame and its distributed owner; if no
inspectable stopped frame exists, the expression is retained for a later stop.
Unrelated watch values and children remain intact and are not reevaluated by the
edit action. Removed watches cannot be recreated by a late prompt submission.

Verification on 2026-10-10:

- 249 unit tests and two integration helpers passed, including captured identity,
  child-row editing, old value/child rejection, cancellation, unchanged/empty
  submissions, running targets, removal during editing, and mouse entry.
- Real GDB and LLDB terminal workflows edited `counter` to `counter + 1`, checked
  `31338`, cancelled a second edit, and continued existing watch/tree/step checks.
  They passed in 13.75 and 8.87 seconds respectively.
- Strict all-target TUI Clippy, formatting, and diff whitespace checks passed.

Watch persistence, details/export, and the remaining distributed-memory TUI
verification are still open. This is not a full-goal completion claim.

#### Oversized timeline-event navigation

The timeline now owns sanitized events and cached grapheme-aligned byte ranges
for wrapped rows. Visual positions use `usize`, and rendering receives only the
visible row slices without Paragraph's 16-bit scroll offset. Width changes
rebuild the row index; normal scrolling reuses it; new events are wrapped once
at the current width. The existing 500-event retention and follow-on-new-event
behavior are unchanged. Individual event text is not truncated.

Wrapping prefers whitespace boundaries and splits overlong words only between
graphemes. An overflowing separator is consumed at the wrap; an already-fitting
word stays on its row. Wide/combining Unicode and tiny terminal layouts remain
covered. A glyph wider than a one-cell pane needs a wider pane to render, but
its text remains retained.

The new rendered regression failed on the old renderer because the tail of one
event was unreachable beyond 65,535 wrapped rows. It now verifies beginning,
tail, and resize navigation. A numbered-row test checks exact positions 65,535
and 65,536 within one event. Cache reuse and retention have direct tests.

Verification on 2026-10-10: 247 unit tests and two integration helpers passed.
The mock full terminal workflow passed in 3.61 seconds; strict all-target TUI
Clippy, formatting, and diff whitespace checks passed. Real debugger workflows
were not rerun for this local timeline-rendering change.

This closes the known single-event timeline scroll limit. It does not close
details/export, watch editing/persistence, or the remaining distributed-memory
TUI verification gap.

#### Extension action label scrolling and distributed recheck

Long extension action labels now have an independent horizontal viewport.
Shift+Left/Right and Shift+horizontal wheel scroll the label; Shift+Home resets
it. Ordinary Left/Right still selects actions, and Up/Down still scrolls data.
Changing the selected action or receiving new capability descriptors resets the
label position. The shared Unicode-aware clipping code preserves grapheme cells.

The new rendered-input regression failed before the fix because Shift+Right
selected the next action. It now reaches the end of a long wide/combining-Unicode
label without changing the selected action or sending backend requests, and
checks keyboard reset, mouse scrolling, and reset on action selection.

Verification on 2026-10-10: 243 unit tests and two integration helpers passed.
All 22 non-greeter terminal workflows passed serially in 105.87 seconds, including
real GDB/LLDB inspection, stepping and register restoration, source ownership,
managed lifecycle, startup settings, and logpoints. Strict all-target TUI Clippy,
formatting, and diff whitespace checks passed. No diagnostic probes were added.

At `f67e7298`, the real greeter auto-attach terminal workflow passed in 48.55
seconds, before the extension-label-only change. It verified both client and
server breakpoints with all canonical threads stopped at each, then exercised
paused-hit navigation. The historical failure table at `51f365fa` above is not
the latest verdict. The test's 15-second individual waits and all-stop assertions
were unchanged, and the test did not overwrite shared discovery configuration.

The single oversized timeline-event limitation, details/export, watch editing
and persistence, and full distributed TUI variable-to-memory composition remain
open. A passing greeter run does not prove every late-event/recovery ordering.

#### Confirmed session termination verification

Topology `K` / `Kill [K]` captures the selected session or a selected thread's
owning session, never a group or the broader execution scope. The scrollable
dialog shows the name, canonical session ID, process ID when known, and DDB
instance. Enter/Esc/n/q cancels; `y` or a left-click on Terminate accepts.
Underlying navigation and execution keys are ignored while confirmation is open.

The session-termination module owns target capture and validation. Confirmation
and the worker check the captured instance, session revision, process identity,
creation timestamp, and live status. The worker fetches a fresh topology snapshot
before using public Execute with SIGKILL and that snapshot's state-revision
precondition. This is the API's optimistic pre-admission check, not a claim that
state cannot change after admission. Rejected confirmation requires a new user
action; it is not silently retargeted or automatically reconfirmed.

Tests cover cancellation, focus/selection changes, ignored execution keys,
removed/changed/exited/replacement sessions, and HTTP request target, signal,
and revision guard. The mock terminal workflow opens the warning and cancels
before continuing its existing debugger interactions. No real process is killed.

Verification on 2026-10-10: 242 unit tests and two integration helpers passed,
including rendered mouse confirmation/right-click rejection and 1x1, 8x5, and
45x14 dialog layouts. The mock full terminal workflow passed in 5.37 seconds.
Strict all-target TUI Clippy, formatting, and diff whitespace checks passed.
Real backend termination was not exercised. The broader adapter-parity goal
remains open, including details/export and watch editing/persistence.

#### Breakpoint file grouping verification

Breakpoint views default to flat logical/member rows. `v` or the header control
switches to full-path file groups. Rendering and keyboard navigation share the
model's presentation order; neither sorts canonical storage nor changes action
targets. Group headers are skipped by keyboard selection and have no breakpoint
mouse hitbox. Functions, addresses, and unresolved locations without source lines
appear under a non-source heading. Toggling back restores snapshot order.

Regression coverage checks full paths with identical basenames, line/ID ordering,
non-source locations, preserved targets, member replacement/reordering across
snapshots, inert header clicks, and keyboard/header-button reset. The mock terminal
workflow toggles grouping and changes breakpoint enabled state through the public
backend before returning to its existing source and inspection checks.

Verification on 2026-10-10: 239 unit tests and two integration helpers passed;
the mock full terminal workflow passed in 5.92 seconds. Strict all-target TUI
Clippy, formatting, and diff whitespace checks passed.

This closes the file-grouping view gap, not the full adapter-parity goal. Real
greeter/GDB/LLDB workflows were not rerun for this presentation-only change.

#### Topology grouping verification

The topology pane defaults to grouped sessions. Its `Flat [v]` / `Group [v]`
header control and focused-pane `v` key switch presentation without backend
requests. Flat sessions sort by display name with opaque session ID as a tie
breaker, and include ungrouped sessions and unresolved group memberships.
Session/thread selections remain attached to their IDs. When flattening a
selected group, its first visible member becomes highlighted while the execution
target remains the group. The status message explicitly reports unchanged scope.

Verification on 2026-10-10:

- 237 unit tests and two integration helpers passed. Coverage includes duplicate
  names, discovery replacement, group-target preservation, rendered mouse
  hitboxes, flat thread-row mapping, and selected thread/frame preservation.
- The mock full terminal workflow passed in 5.20 seconds. It clicks the rendered
  header, checks the stopped source marker, restores grouping with the keyboard,
  and continues the existing breakpoint, source, and inspection workflow.
- Strict all-target TUI Clippy, formatting, and diff whitespace checks passed.

This change does not close breakpoint grouping or the full adapter-parity goal.
Real greeter/GDB/LLDB workflows were not rerun for this presentation-only change.

#### Explicit expression inspector verification

`E` and the source-header `Inspect [E]` button open a one-shot expression prompt.
The inspector captures the selected stopped frame before the user types, sends
the public `HOVER` context, and renders one expandable `Inspect:` row in Variables.
It uses the existing variable child and memory operations. Delete dismisses its
root and children; frame invalidation clears it; late or superseded results
cannot reopen it. It is not part of retained watches and does not auto-refresh.

The evaluation worker is shared with watches, but the inspector's lifetime and
request identity are separate. This keeps ownership and stale-result policy in
the expression-inspection module. Queue rejection now reports an evaluation's
own identity/error for inspectors and watches instead of leaving a loading value.
No automatic mouse-motion evaluation was added.

Verification on 2026-10-10:

- 235 unit tests and two integration helpers passed. New tests cover keyboard
  entry, the rendered mouse hitbox, cancellation without evaluation, distributed
  owner capture, late scope updates, child expansion, watch-refresh independence,
  dismissal, stale/superseded replies, and scoped queue rejection.
- The worker HTTP contract test asserts the exact HOVER context, owner thread,
  and captured frame, then checks the expandable result.
- Both real GDB and LLDB terminal workflows enter `request`, expand its inspection
  result, observe the `headers` child, and dismiss the result before proceeding
  through existing watches and memory interactions.
- All 22 non-greeter terminal workflows passed serially in 105.26 seconds.
  Strict all-target TUI Clippy, formatting, and diff whitespace checks passed.

The greeter workflow was not rerun for this change. Grouping/detail views,
confirmed session termination, editor-hosted watch behavior, and the remaining
clipped-content and distributed TUI verification items are not closed by this work.

#### Real distributed-caller memory verification

The real distributed-backtrace matrix now also exercises the authenticated v2
inspection API. For each four-session GDB/LLDB case it obtains the public
distributed backtrace from the leaf, selects a non-top root caller frame, and
asserts that its owner differs from the leaf thread. It then lists that frame's
locals, expands a known fixture struct, and reads its field address through the
owning thread. Typed ProtoJSON decoding checks all eight bytes against
`9501_u64`, rather than accepting merely nonempty memory output.

The fixture retains the original context-restoration checks. After restoring and
resuming callers, the test attempts to inspect the retained frame again and
requires `EXPIRED` or `FAILED_PRECONDITION`, not an arbitrary API error.

Final verification on 2026-10-10: all four real distributed-backtrace tests passed
in 9.70 seconds, covering local and four-session GDB/LLDB cases. Strict Clippy for
the integration target, formatting of the touched Rust files, and diff whitespace
checks passed. Only the fixture, test setup, and integration assertions changed.

This is real API evidence combined with the existing TUI ownership unit tests;
it is not a claim that a complete distributed-caller TUI mouse/keyboard workflow
has been exercised. That UI composition and the remaining parity work stay open.

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
