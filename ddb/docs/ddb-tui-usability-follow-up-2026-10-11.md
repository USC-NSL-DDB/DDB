# DDB TUI usability follow-up

This is the sixteen-requirement follow-up on `codex/tui-next`. It is separate
from the earlier eleven-requirement audit. The full follow-up is still open.

## Navigation decision

The user chose direct Alt-number pane shortcuts, not a prefix sequence.
Numbers are stable across layouts and optional panes:

| Shortcut | Pane |
| --- | --- |
| Alt+1 | Topology |
| Alt+2 | Breakpoints |
| Alt+3 | Source |
| Alt+4 | Stack |
| Alt+5 | Variables |
| Alt+6 | Registers |
| Alt+7 | Memory |
| Alt+8 | Timeline |
| Alt+9 | Output |
| Alt+0 | Extensions |

`z` expands/restores the focused pane. Escape restores an expanded layout.
Shortcuts respect modal and prompt input. Selecting hidden Timeline or Output
opens it. Unavailable inspection panes report their prerequisite.

The layered prefix configuration/settings-page requirement was explicitly
conditional on choosing a prefix. That condition is false. No prefix or prefix
configuration system was added. Source search has a separate configurable
default through `--source-search exact|fuzzy`.

## Requirement audit

| # | Requirement | Status and evidence |
| --- | --- | --- |
| 1 | Source-relevant breakpoint sessions/groups from a reusable DDB API | Implemented. `DebuggerService.ResolveSourceTargets` owns exact-path membership discovery; HTTP, gRPC, generated contracts/SDK types, and TUI consume it. Content readability is not a membership prerequisite. |
| 2 | Keyboard pane expansion | Implemented with the existing `z`/Escape behavior, direct pane selection while expanded, visible numbered titles, and interaction tests. |
| 3 | Quick numbered pane switching | Implemented with fixed Alt+1…9/0 shortcuts. Optional panes never renumber other panes. |
| 4 | Layered prefix configuration and provenance/settings UI | Conditional, not triggered by the user's direct-shortcut decision. |
| 5 | Remove duplicate paused labels | Implemented. Logical breakpoint rows show the current paused count; member rows show hit markers, not another paused label. Both grouping modes share this renderer. |
| 6 | Ctrl+u/Ctrl+d half-page source movement | Implemented. Uses actual pane height and the existing source-window request path. Ctrl+d never falls through to plain `d` stack refresh. |
| 7 | Organized searchable help | Implemented. Categorized shortcut catalog; `/` searches keys, categories, and descriptions with case-insensitive word matching. Prompt editing is separate from help navigation. |
| 8 | Exact/fuzzy source text search, configurable default | Implemented. `/`, Ctrl+f in the prompt, `n`/`N`, mouse controls, and `--source-search exact\|fuzzy`. Whole-file matching runs at the source owner and returns version-pinned context. Local and remote providers share the indexed matcher. |
| 9 | Switchable breakpoint-hits-only pane | Implemented. `H` / Hits-All button switches the persistent Alt+2 pane to current stops grouped by DDB group/session. Stop-identity selection, stale-click rejection, read-only mutation guards, lifecycle/interaction tests, and real GDB/LLDB frame/source/watch navigation are covered. |
| 10 | Indicate frames whose source is unavailable | Implemented. Unusable locations are dimmed; real source loads distinguish unchecked, loading, readable, and failed states per backend/session/path. Failed loads dim the function name and retain a visible reason; retries recover. Filesystem, stale-result, lifecycle, style, and native-DDB terminal coverage are recorded below. |
| 11 | Measure and improve pause/resume/all-stop refresh latency | Open. Do not infer improvements from test duration. Separate backend operation/event timing from frontend inspection/source/render timing. An intermittent real-greeter source timeout remains recorded below. |
| 12 | Discoverable keyboard navigation from breakpoint to paused frame | Implemented in help and hints. Alt+2, choose a breakpoint, P, choose a paused hit, Enter; Escape cancels. |
| 13 | One-click topology expansion/collapse and individual signaling | Implemented. Disclosure clicks, Space/Enter and Left/Right fold/navigate groups/sessions. Folding preserves control scope and inspected thread. Mouse rows carry resource identities; session-label selection plus `s` submits a canonical single-session signal. Unit and two-session terminal coverage verify the target and resolved count. |
| 14 | Show only execution actions supported for selected scope | Implemented. Toolbar filtering uses DDB's typed `execution_action_capabilities`, not only the global action list. Tests cover thread/session/group and no target. |
| 15 | General pretty-printing solution, including greeter C++ string | Open. Existing default printer setup does not establish that greeter `prefix` is pretty-printed. Diagnose debugger formatter registration and evaluation separately; do not hard-code C++ structure parsing in the frontend. |
| 16 | Visible pending/completed/error feedback for controls and other operations | Implemented for execution controls and backend mutations. Identity-correlated queue/admission/result feedback is independent of routine status, survives snapshots and navigation, reports unconfirmed outcomes honestly, and has model, interaction, serialized-worker, and terminal coverage. |

## Source-target API guarantees

The request contains an exact debugger path, optional target restriction, and
refresh flag. Frontends apply source-workspace mappings before sending it. A
successful response contains only proven session matches and their groups,
matching-session counts, warnings, and an explicit completeness flag.

Running targets are never interrupted for discovery. Cached positive or negative
membership remains usable. Unknown membership or discovery failures make the
response incomplete. Empty incomplete results must not be presented as proof of
no owners. Refresh applies to stopped sessions only. Group installation retains
ordinary DDB inheritance; its matching count does not redefine group membership.

Discovery is bounded, single-flight per session, cancellation/deadline-aware at
the API request boundary, and revalidated against session/group lifecycle before
publication. Result overflow is a typed resource-exhaustion error. No broadcast
scope is returned. See [source workspaces](api/source-workspaces.md).

The TUI runs this lookup separately from serialized controls. Picker generations
and backend instance identities prevent old replies from changing a reopened
picker. It does not fall back to listing every session or filtering ownership
locally. Function breakpoints retain their symbol-based scope picker.

## Audit and verification

Implementation commits:

1. `8d7b090c`: canonical source-target API, application implementation, catalog
   membership queries, generated contracts and SDK types, transport bindings,
   documentation, and backend integration tests.
2. `d770bfa2`: asynchronous source-aware TUI picker, stale-response guards,
   in-modal states, queue-overload handling, and picker/terminal readiness tests.
3. `ba31117a`: direct pane shortcuts, half-page source navigation, searchable help,
   scope-aware toolbar, and breakpoint/frame presentation changes.

These commits were staged by explicit file/hunk allowlists. Unrelated navigation
and help edits were excluded from the picker commit. There are no v1 fallbacks or
compatibility shims for this lookup.

Verified during this batch:

- Four `api_v2_sources` integration tests passed.
- The catalog unknown/negative/positive membership and retirement test passed.
- `cargo check -p ddb --features grpc-preview` passed.
- `ddb-api-codegen --check` reproduced generated artifacts exactly.
- Backend workspace and TUI formatting checks passed.
- TUI unit and terminal results are finalized in the verification addendum.

The first full non-greeter terminal run exposed assertions affected by pane
labels and global hints. Pane-name hints were removed from the global footer;
the focused Breakpoints assertion now includes its visible number. The following
run passed both real GDB and LLDB workflows. Its remaining mock title assertion
passed after that explicit numbered-title correction.

The first real greeter run timed out waiting for the source cursor at line 59
before breakpoint creation. The failure screen showed a source request still
loading. An unchanged isolated rerun passed in 33.96 seconds. This is not evidence
of a fixed latency bug. Keep the failing observation in the requirement 11
investigation and reproduce it with request timing before changing timeouts.

## Remaining implementation and quality checks

Continue in independently reviewable commits:

This list records the original sequence. Source search and the persistent
hits/topology work are now implemented in the later addenda. Source availability,
operation feedback, measured latency, pretty-printing, and the final audit remain.

1. Source text search: exact default, explicit fuzzy toggle, configurable default,
   repeated forward/backward search, and clear search feedback. Cover Unicode,
   empty/no-match queries, source changes, large files, and unloaded source
   windows. Do not silently restrict a file search to the visible window.
2. Breakpoint hits and topology: a switchable persistent hits-only view grouped by
   owning session/group, folding controls with mouse and keyboard parity, and
   stable selected targets. Concurrent stops, resume, removal, reconnect, and
   breakpoint replacement must not leave stale hits or redirect signals.
3. Source availability: distinguish absent locations from unreadable/missing
   content using authoritative source metadata. Avoid per-frame serial discovery
   requests that worsen stack-refresh latency.
4. Execution feedback and latency: correlate queued/pending/completed/failed
   operations with explicit request/operation identity. Feedback must remain
   visible despite snapshot updates. Measure cold and warm continue/next/pause
   and all-stop flows with backend, event, inspection, source, and render timing.
   Keep deadlines and correctness assertions intact while optimizing.
5. Pretty-printing: reproduce `prefix` with the real instrumented greeter and
   inspect GDB/LLDB formatter availability, setup completion, and evaluation
   results. Keep formatter policy backend-neutral and raw child inspection
   available. Add debugger-backed regression tests for formatted values.
6. Final audit: unit tests, strict all-target Clippy, generated-contract drift and
   formatting checks, full terminal suite, real GDB/LLDB workflows, and the real
   auto-attached distributed greeter. Inspect each staged diff and commit the
   exact verification results. Do not mark this follow-up complete while any
   unconditional requirement above remains open or partial.

## Verification addendum

Final checks on the combined implementation batch:

| Check | Result |
| --- | --- |
| TUI unit tests | 288 passed in 2.16 seconds |
| TUI non-ignored integration helpers | 2 passed |
| Full non-greeter terminal suite | 27 passed in 85.42 seconds |
| Real greeter auto-attach/all-stop | Isolated rerun passed in 33.96 seconds; initial source-loading timeout remains an intermittent finding |
| Strict all-target TUI Clippy | Passed, with no warning suppressions added |
| TUI formatting | Passed |
| Backend workspace formatting | Passed using `cargo fmt --manifest-path ddb/Cargo.toml --all -- --check` |
| Backend source API integration | 4 passed |
| Catalog membership/retirement unit test | Passed |
| Native gRPC preview compilation | Passed |
| Generated contract drift | Exact reproduction |
| Staged diff checks | Passed for each commit |

The terminal suite includes managed GDB/LLDB launch, real debugger workflows,
distributed caller memory, source ownership, function breakpoints, logpoints,
source exploration without sessions, crash/terminal cleanup, and watch persistence.
Passing these checks does not close the open or partial requirements above.

Reproduce the main frontend gates from the repository root:

```sh
cargo test --manifest-path ddb-tui/Cargo.toml --locked --quiet
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --all-targets -- -D warnings
cargo fmt --manifest-path ddb-tui/Cargo.toml -- --check
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  -- --ignored --skip greeter_auto_attach --test-threads=1
```

The real greeter test additionally requires the existing instrumented workspace,
`ddb_runapp`, and a free configured loopback discovery port. It refuses to replace
an occupied broker and must not overwrite the shared discovery configuration.

## Whole-source search follow-up

Reviewable commits:

1. `f2ec2d41`: shared immutable indexed matcher, exact/fuzzy modes, Unicode
   scalar columns, whole-file directional wraparound, and library tests.
2. `c8f576e8`: canonical `DebuggerService.SearchSource`, HTTP/gRPC transports,
   typed Rust client, generated contracts and SDK types, authorization and
   bounded source I/O, pinned match/context responses, API guide and tests.
3. `3fa8ffa2`: asynchronous TUI search lane, keyboard/mouse controls,
   configurable default, stale-response and cancellation guards, persistent
   feedback, grapheme-safe highlights, correct horizontal positioning and
   search-bar-aware paging, unit and terminal regression tests.

Exact search is a case-sensitive substring. Fuzzy search is a Unicode lowercase
subsequence, not edit distance. Searches return one match per matching line.
The source owner scans the whole bounded file once; the TUI does not fetch every
240-line source window. Matching and returned context use the same immutable
document version. No-match responses also replace stale source text.

Final checks for this follow-up:

| Check | Result |
| --- | --- |
| Shared source-workspace library | 19 tests passed; strict all-target Clippy passed |
| Backend source API integration | 5 passed in 1.04 seconds |
| Native gRPC preview compilation | Passed |
| Generated contract drift | Exact reproduction |
| TUI unit tests | 299 passed in 2.16 seconds |
| TUI non-ignored integration helpers | 2 passed |
| Full non-greeter terminal suite | 29 passed in 88.71 seconds |
| Final local/remote source-search terminal checks | Both passed in 1.60 seconds after final hit-area changes |
| Strict all-target TUI Clippy | Passed |
| Backend/TUI formatting and diff checks | Passed |
| Real greeter auto-attach/all-stop | Failed before breakpoint creation, waiting for source cursor at server line 59 |

The greeter failure reproduced the earlier intermittent source-loading finding.
The failure screen showed `Loading source` / `Resolving or reading source`,
while stack, scopes, and registers were populated. The test ran for 19.80 seconds
and its source wait limit was 15 seconds. No source search had been requested.
This is not evidence of a search regression or a diagnosed root cause. Do not
close requirement 11 or advertise full real-greeter verification on this basis.
The next diagnosis should minimize this auto-attach-to-source-readiness path,
measure its request/inspection stages, and establish a regression loop before
changing production scheduling or timeouts. The shared discovery configuration
was not modified, and only test-owned processes were cleaned up.

Reproduce search-specific terminal coverage:

```sh
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  whole_source_search_terminal_workflow -- --ignored --test-threads=1
```

At the end of the search batch, requirements 9, 11, 13, 15, and 16 remained open.
Requirement 10 remained partial. The current statuses are in the audit table.
The sixteen-requirement goal is not complete.

## Source-readiness latency investigation

The direct Alt-number navigation decision remains unchanged. No prefix or
layered prefix configuration was introduced.

The real auto-attach test now has a minimized source-readiness variant. It shares
the original broker, backend, auto-attached server, and TUI setup, then exits as
soon as the requested server line is visible. The original all-stop workflow
continues past that same assertion. Both retain the existing 15-second source
deadline. Neither changes the shared discovery configuration or takes ownership
of an existing broker.

Temporary request and command-stage probes established the following:

- One successful run spent 5,706 ms in `ResolveSource`, followed by approximately
  4 ms in `ReadSource`. The TUI applied the same source generation it requested.
- Failing minimized runs also issued a single source generation. DDB admitted,
  wrote, and acknowledged the source-list command, but no corresponding debugger
  result arrived before the source deadline. Earlier command state barriers
  completed. There was no observed stale-generation rejection or cancellation
  of the manual source request.
- During a successful sampled run, the test-owned GDB process consumed CPU and
  briefly waited in filesystem RPCs. This supports investigating debugger source
  discovery; it does not quantify how much each filesystem operation contributed.
- A separate GDB/MI measurement loaded the real greeter binary before timing each
  query. Full, directory-filtered, and basename-filtered source enumeration took
  3.420, 3.512, and 3.452 seconds respectively. Smaller response payloads alone
  did not remove the cold-query cost in this experiment.

The implemented optimization is deliberately narrower than a cold-path fix.
`ResolveSource` previously asked for a complete listing even when an exact path
already had positive or negative membership evidence in a partial catalog. It
now uses that evidence directly. Unknown stopped-session paths use the existing
single-flight path discovery, with at most four session requests in flight and
the request deadline applied to collection. Unknown running-session paths never
trigger discovery. Ambiguous ownership still requires an explicit session.

The application-level regression seeds only a partial catalog and deliberately
registers no debugger in the fixture router. It failed on the original code with
`BackendFailed`, then passed with the optimization. Both positive and negative
cached membership avoid backend I/O, without falsely completing the catalog.

The matching optimized backend still reproduced a cold source-readiness timeout.
Therefore requirement 11 remains open. All temporary tagged probes were removed;
no production deadline or correctness assertion was relaxed. Another observation
for subsequent correctness work is that GDB's `--dirname` operand is a regular
expression, while the current resolver passes a quoted literal directory and
the LLDB bridge treats it as a literal prefix. Directories containing regular
expression metacharacters need explicit cross-backend coverage and aligned
filter semantics. See the [GDB/MI file command documentation](https://www.sourceware.org/gdb/current/onlinedocs/gdb.html/GDB_002fMI-File-Commands.html).

Reproduce the minimized real-app regression:

```sh
DDB_GREETER_WORKSPACE=/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld \
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  greeter_auto_attach_source_readiness_terminal_workflow \
  -- --ignored --test-threads=1 --nocapture
```

This test is intentionally capable of exposing the unresolved cold-discovery
delay. A passing rerun is not proof that the intermittent problem is fixed.

Final checks after removing every temporary probe:

| Check | Result |
| --- | --- |
| Application service unit tests | 28 passed in 0.18 seconds, including the partial-membership regression |
| Source API integration tests | 5 passed in 0.93 seconds |
| Matching backend debug binary | Built successfully |
| TUI unit tests and integration helpers | 299 unit tests and 2 helpers passed |
| Strict all-target TUI Clippy | Passed |
| Strict DDB-only Clippy with `--no-deps` | Passed in 32.59 seconds |
| Strict backend Clippy including path dependencies | Failed in untouched `gdbmi` with 38 lint errors; no broad lint cleanup included |
| Backend/TUI formatting and diff checks | Passed |
| Cleaned original real-greeter all-stop workflow | Failed waiting for the source cursor at line 59, 19.87 seconds total |
| Cleaned minimized real-greeter source workflow | Reproduced the same timeout, 18.96 seconds total |
| Temporary probe search | No `[DEBUG-source-11]` instrumentation remains in backend/frontend source or tests |

The optimization and its application regression are isolated in commit
`059fafa1`. The minimized real-app regression and this evidence are separate
test/documentation commits. None of these results closes the remaining open
requirements or establishes a fully passing real distributed-app workflow.

## Persistent breakpoint hits and topology folding

Requirement 9 now has a persistent mode in the existing numbered Breakpoints
pane. `H` and the Hits/All footer button switch it without changing pane numbers
or the all-breakpoints selection. Rows come from canonical thread execution
states, not cumulative counters. DDB group and session headers organize the
currently hitting threads. Automatically paused all-stop peers remain excluded.

The selected hit retains breakpoint, member, session, thread, execution-state,
and revision identity. Resume, replacement, or deletion cannot redirect Enter
to another row. Mouse hit areas retain that same stop identity and revalidate it
at click time. A disconnected backend has no verified live hits. Reconnecting
does not silently restore a discarded selection. Navigation loads the owning
thread's ordinary stack/source/values without changing the control scope.
Mutation keys cannot act on the hidden all-breakpoints selection.

Requirement 13 now has identity-keyed group/session folding. Disclosure clicks,
Space/Enter, and Left/Right share the model's fold/navigation methods. Folding
does not execute debugger commands or change the inspected thread/control
target. A hidden selection is represented by its nearest visible ancestor.
Explicit hit navigation unfolds its ancestors. Preferences survive flat/grouped
view changes and refreshes, are pruned for removed resources, and reset when the
DDB server instance changes.

Topology labels select canonical resources; disclosure arrows only fold. Mouse
areas carry the rendered resource identity instead of using a mutable row
offset. Reordering cannot turn a session click into a different session/group
target. Hidden or removed rendered rows are rejected. The two-session terminal
test folds a group and session with keyboard/mouse input, selects a signal for
one session, and verifies both the completed signal receipt and the canonical
operation target with `resolved_target_count == 1`.

Plain Left/Right now navigate Topology. Shift+Left/Right and the horizontal wheel
scroll it; Home resets the horizontal position. Help and README document this
deliberate conflict resolution. Session identity diamonds remain separate from
disclosure arrows. The terminal fixture's top-of-list acknowledgement now checks
the selection-marker column, so a collapsed disclosure cannot falsely satisfy
it. This is test-driver correctness, not a production timeout adjustment.

The shared hit and topology model methods keep keyboard and mouse behavior at
the same interface. Tests cover concurrent hits, stale stops/clicks, hidden
mutation protection, backend replacement/disconnection, empty/tiny layouts,
flat/grouped/unresolved/ungrouped topology, folding during signal selection, and
resource retirement. No compatibility API fallback or backend schema change was
needed for these features.

Reviewable implementation and test commits:

- `7ea6370b`: persistent hit-view implementation and model/interaction tests.
- `79f3c044`: real GDB/LLDB terminal navigation through the persistent hit view.
- `d20fb5dd`: topology folding, identity-based selection, and interaction tests.
- `f20743d8`: two-session terminal folding and exact single-session signal proof.

This audit and the user guide are a separate allowlisted documentation commit.

Final checks on the combined batch:

| Check | Result |
| --- | --- |
| TUI unit tests | 310 passed in 2.15 seconds |
| Non-ignored terminal helpers | 2 passed |
| Strict all-target Clippy | Passed in 5.33 seconds |
| Complete non-greeter terminal suite | 30 passed in 88.14 seconds |
| Real function-breakpoint/hit navigation | GDB and LLDB passed, both isolated and in the full suite |
| Two-session fold and signal workflow | Passed in the full suite; isolated run passed in 0.98 seconds |
| Formatting and staged diff checks | Passed |
| Real auto-attached greeter | Not rerun in this batch; the previously reproduced cold-discovery timeout remains unresolved |

Reproduce the relevant terminal checks:

```sh
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  real_function_breakpoint_terminal_workflow -- --ignored --test-threads=1
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  topology_folding_and_single_session_signal_terminal_workflow \
  -- --ignored --test-threads=1
```

At the end of the hits/topology batch, requirements 11, 15, and 16 remained open,
and requirement 10 remained partial.
The source-directory regexp semantic mismatch recorded above also remains a
correctness follow-up. The sixteen-requirement goal is not complete.

## Operation feedback addendum

Requirement 16 now has a bounded feedback module separate from the general
status message and backend pending-command count. A local monotonically
increasing intent number exists before a request enters the worker queue.
The worker reports when it starts admission, the canonical DDB operation ID
when admitted, and the final receipt when available. It continues to use the
current v2 mutations and SDK operation wait, without a compatibility fallback,
automatic retry, or changes to execution ordering.

The visible states are queued, pending admission, pending, completed, error,
cancelled, and outcome unconfirmed. The latest submitted intent remains in the
footer; finishing an older operation cannot replace it. A count identifies older
pending work even if the latest intent has completed. Full target identities and
operation IDs are retained in the wrapped Timeline. Target labels are captured
at submission instead of being inferred from the currently inspected frame.
Expressions, assignment values, and raw-command text are not copied into the new
feedback labels.

Covered actions are Continue/Interrupt/stepping, jumps, signals, session
termination, breakpoint creation/deletion/enablement, expression evaluation,
value/register assignment, native commands, extension actions, and local
logpoint configuration. Existing source/search/picker and value-inspection
loading/error states remain independent. This is not a new general task runner
or an operation cancellation UI.

Queue rejection explicitly says the request was not sent. An error after DDB
admission leaves the result unconfirmed instead of declaring backend execution
failed. Connection loss cannot prove failure either. A same-instance final
receipt may resolve the uncertainty; backend replacement retires previous
intents so late replies cannot complete them. Canonical failed/cancelled receipts
and failed fanout members produce visible errors. No failed member is represented
as an all-success result.

History is limited to 64 intents. Active intents are not silently evicted;
overload rejects the new command before sending. General status and prompt input
remain visible alongside feedback, including in the tiny layout. Routine
snapshots, inspection statuses, automatic all-stop receipts, and stale value-edit
contexts do not determine the tracked operation's state.

Verification includes five feedback-model tests, four interaction/rendering
tests, and two serialized-worker tests. Controlled v2 admission and operation
polling prove that admission is reported before completion, the second control
stays queued behind the first, distinct intents retain distinct operation IDs,
and a rejected admission never emits a success receipt. Existing request tests
still assert their original canonical targets and payloads after unwrapping the
internal tracking envelope.

The mock full-terminal workflow now checks that completed Next feedback remains
in the current footer after a snapshot refresh and source navigation. Existing
real GDB/LLDB terminal workflows also exercise the tracked mutations.

Reviewable commits:

- `5821f3be`: feedback module, queue/worker/UI integration, model/interaction/
  serialized-worker tests, and unchanged target/payload assertions through the
  internal request envelope.
- `d1ca0d1c`: durable completed-Next footer assertions in the full terminal
  workflow.
- This audit and the README are a separate allowlisted documentation commit.

Final checks on the committed implementation and terminal regression:

| Check | Result |
| --- | --- |
| TUI unit tests | 321 passed in 2.27 seconds |
| Non-ignored terminal helpers | 2 passed |
| Strict all-target Clippy | Passed in 6.14 seconds |
| Complete non-greeter terminal suite | 30 passed in 89.34 seconds |
| Real GDB and LLDB workflows | Passed in the full suite |
| Focused mock full-terminal workflow | Passed in 4.51 seconds before the final compact-layout adjustment; passed again in the final full suite |
| Formatting and staged diff checks | Passed |
| Real auto-attached greeter | Not rerun; the recorded cold-source discovery timeout remains unresolved |

Reproduce the checks:

```sh
cargo test --manifest-path ddb-tui/Cargo.toml --locked
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  -- --ignored --skip greeter_auto_attach --test-threads=1
```

At the end of the operation-feedback batch, requirements 11 and 15 and the
source-directory regexp follow-up remained open, and requirement 10 was partial.
Control-target retirement still needed a regression check: thread reconciliation
could replace a disappeared explicit target with the newly inspected thread.
That batch captured queued targets correctly but did not change reconciliation.
The fixes below supersede those remaining-work notes for target safety and
requirement 10.

## Target retirement and source-read evidence addendum

The control-target follow-up is fixed in `1a5714a2`. The regression selected a
session/thread in A, removed A while B survived, and observed the target accessor
return B. It failed twice before the implementation change, in 0.23 and 0.00
seconds. `apply_threads` was overwriting the unavailable explicit target with
the recovered inspection thread.

The initial default is still established once. Subsequent retirement preserves
the target identity and disables execution until explicit reselection. The scope
label explains that it is unavailable. Session sets and recursive compound
targets validate their members. Disappeared IDs cannot automatically reactivate
controls if they reappear; replacement instances cannot revive old targets.
Inspection selection is allowed to recover independently. Scope cycling now
uses the captured control thread's owning session instead of an unrelated
inspected thread.

Four new lifecycle/scope regressions passed. Existing advertised-scope and
extension-action fixtures now create the topology resources their targets
reference. The asynchronous extension assertion has a two-second failure bound
instead of waiting indefinitely. The target-only batch passed 325 unit tests,
strict all-target Clippy, and the two-session signal terminal workflow in 1.30
seconds. No debugger API or compatibility behavior changed.

Requirement 10 now uses actual source-read evidence, not inference from a
reported file/line. The frame label distinguishes these states:

| Evidence | Presentation |
| --- | --- |
| No usable file/line, including empty paths, zero lines, and API-overflow lines | Dim function name and `no source location` |
| Reported location, no retained read evidence | `source unchecked` |
| Current source request for this owner/path | `loading source` |
| Accepted successful source read | Normal function name, no availability warning |
| Accepted failed load | Dim function name, `source load failed`, and the reason on the scrollable row |

The Source pane retains the detailed error. Enter/F8 remains available to retry;
failed loads are not treated as a permanent filesystem diagnosis. Network,
permission, missing-file, invalid-UTF-8, and queue failures can prevent loading
without changing debugger-provided location metadata. No unrelated owner is
marked unavailable on an ambiguous group/broadcast resolution failure.

Evidence is bounded to 128 session/path entries and belongs to the current DDB
instance. It is discarded on session retirement or instance replacement.
Generation checks cover request registration, accepted browser results, and
queue rejection. Stale/cancelled results and foreign backend owner hints cannot
alter current-frame evidence. Identical debugger paths in different sessions are
independent. A successful retry replaces the prior failure. Rendering compares
borrowed identities rather than allocating an owner/path key per frame.

No eager per-frame source enumeration or content reads were introduced. Frames
without evidence remain explicitly unchecked. This avoids adding source-query
traffic to the pause/stack-refresh path; it is not a claim that requirement 11's
latency investigation is complete. Explicitly revealing an unusable location
now clears the previous source view instead of silently leaving it visible.

Six new tests cover actual missing/invalid-UTF-8 local files and restoration
through the source worker, per-owner isolation, stale/cancelled replies,
ambiguous/foreign ownership, backend/session replacement, invalid locations,
bounded evidence, small layouts, and the actual dimmed function-name color.
The new native-DDB terminal workflow reports a file/line for a genuinely absent
file, checks the stack failure label, then restores the file and requires a
successful retry and execution marker. An unchanged old failure screen is not
accepted as a retry acknowledgement.

Source retry also clears its own still-displayed status error after a successful
read. It does not erase a different action's error. Help explicitly describes
Enter in Stack and F8 as source-retry paths.

One final full-suite run passed 30 workflows but failed the existing SIGTERM
fixture before SIGTERM was sent. It waited for the transient `managed DDB`
startup status, while the captured screen already showed a connected debugger.
The unchanged isolated test passed in 0.46 seconds, confirming the predicate's
timing sensitivity. Managed-startup waits now acknowledge the persistent
connected state in the current frame. The owned-process, terminal-restoration,
and stopped-session assertions remain intact; no deadline increased. The fixed
SIGTERM workflow passed in 0.50 seconds and again in the final complete suite.
This is a test-readiness correction, not a claimed production shutdown fix.

Reviewable commits:

- `1a5714a2`: target-retirement safety, correct owner-based scope cycling, and
  lifecycle/interaction regressions.
- `9aa00324`: source-read evidence, UI indicators and retry fixes, searchable help,
  and filesystem/model/interaction tests.
- `2411b6cd`: actual native-DDB missing-file restoration/source-marker proof and
  persistent managed-startup readiness assertions.
- This audit and the README are a separate allowlisted documentation commit.

Final combined checks:

| Check | Result |
| --- | --- |
| TUI unit tests | 331 passed in 2.17 seconds |
| Non-ignored terminal helpers | 2 passed |
| Strict all-target Clippy | Passed in 2.05 seconds |
| Complete non-greeter terminal suite | 31 passed in 89.92 seconds |
| Native-DDB missing-file recovery | Passed in the final suite; focused recovery run passed in 1.03 seconds |
| Real GDB/LLDB, managed launch/attach, signals, source ownership, and distributed inspection workflows | Passed in the complete suite |
| Formatting and staged diff checks | Passed |
| Real auto-attached greeter | Not rerun; its recorded cold-source discovery timeout remains unresolved |

Reproduce the new terminal test:

```sh
cargo test --manifest-path ddb-tui/Cargo.toml --locked --test e2e_mock \
  unreadable_source_is_identified_on_its_stack_frame_terminal_workflow \
  -- --ignored --test-threads=1
```

At the end of the source-availability batch, requirements 11 and 15 and the
source-directory regexp follow-up remained open.

## Literal source-directory discovery addendum

The source-directory regexp follow-up is now fixed. Requirement 1 still uses
the canonical source-target API; no frontend ownership filtering or fallback
API was introduced.

### Reproduction and cause

The new real-GDB regression first failed in 0.36 seconds. An unfiltered listing
contained the compiled source, but `--dirname` with its literal `src[x]` parent
returned an empty list. This could incorrectly cache negative ownership.
[GDB documents that this operand is a regular expression](https://www.sourceware.org/gdb/current/onlinedocs/gdb.html/GDB_002fMI-File-Commands.html).
A separate isolated GDB 14.2 probe confirmed basic-regex behavior for this
command. An extended-regex directory boundary was not valid here.

The provider now generates an escaped, anchored exact-parent-directory
expression. Single-character classes represent punctuation whose backslash
semantics differ between basic regex and Python regex. Full-catalog requests
remain unfiltered. Empty relative parents do not become root-only queries;
root paths and trailing separators have explicit tests.

The LLDB bridge previously interpreted the operand as a literal realpath.
It now applies the generated expression to the source's directory, not its
basename. Missing and malformed expressions are errors, not empty successful
listings. The shared generated-pattern convention is tested through its actual
command tokenizer. This does not claim general equivalence between GDB's basic
regex language and Python's regex language for arbitrary native commands.

### Unicode and malformed MI values

The six-directory native test also exposed a separate result-value decoder
defect. Bytewise conversion corrupted literal UTF-8, and octal byte escapes
were not decoded. Two minimized parser regressions failed in 0.06 seconds;
truncated quoted input also panicked.

Result-value strings now decode C escapes into bytes before UTF-8 validation.
Invalid byte sequences, oversized escapes, and truncated strings return typed
errors. The backend test that previously expected a panic now requires safe
rejection. Stream-record handling is unchanged. The source regression verifies
paths through the production MI parser and source decoder, not raw JSON-like
text comparisons.

### Verification and audit trail

| Check | Result |
| --- | --- |
| MI parser crate | 28 tests passed in 1.32 seconds; 4 doc tests passed in 0.36 seconds |
| Backend unit suite on the final tree | 339 passed in 0.40 seconds; 2 explicitly ignored |
| Real-GDB literal-directory regression | Passed in 1.02 seconds across six compiled fixtures |
| LLDB bridge suite | 18 passed in 0.015 seconds |
| Strict backend test-target Clippy, `--no-deps` | Passed in 14.54 seconds |
| Native terminal suite against rebuilt backend | 31 passed in 98.82 seconds; 4 filtered out |
| Real GDB and LLDB | Stepping, watches, function breakpoints, logpoints, distributed caller memory, and managed launch/attach passed in that terminal suite |
| Formatting and staged diff checks | Passed |
| Real auto-attached greeter | Not rerun in this batch; the recorded cold-discovery timeout remains unresolved |

The parser crate's broader Clippy scan completed, but crate-wide strict lint
cleanliness is not claimed. It still reports existing warnings outside the new
decoder/tests, including 38 library warnings. No unrelated lint cleanup was
included.

The real-directory regression is explicitly invoked by the Rust CI workflow,
which already installs GDB and runs the LLDB bridge suite. It covers brackets,
periods, plus signs, spaces, parentheses, braces, anchors, alternation characters,
question marks, stars, backslashes, quotes, and Unicode. Exact-parent filtering
excludes sibling and descendant directories. Complete catalog queries remain
available when a complete listing is required.

Auditable commits, staged by explicit file allowlists:

1. `888849f2`: MI string decoding, focused regressions, and safe backend error
   handling assertions.
2. `826135c2`: literal parent-directory discovery, LLDB semantics, real-GDB and
   bridge regressions, CI invocation, and test documentation.
3. This documentation-only audit commit records the evidence and limitations.

No generated API contract changed. Resolver concurrency, lifecycle validation,
cache bounds, and cancellation policy are unchanged. The isolated throwaway
probe was removed; no temporary debug instrumentation remains in these changes.

Requirements 11 and 15 remain open. These correctness fixes do not establish a
latency improvement or prove greeter C++ pretty printing. The sixteen-requirement
goal is not complete.
