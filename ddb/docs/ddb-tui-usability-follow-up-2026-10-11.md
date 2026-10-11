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
configuration system was added. The separate source-search requirement still
needs a configurable exact/fuzzy default.

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
| 8 | Exact/fuzzy source text search, configurable default | Open. File-catalog search is not source text search and does not satisfy this requirement. |
| 9 | Switchable breakpoint-hits-only pane | Open. The existing `P` hit-picker modal is not a persistent hits-only view. |
| 10 | Indicate frames whose source is unavailable | Partial. Frames without a file/line are dimmed and labeled `no source location`. A file/line does not prove source content is readable; missing/unreadable files still need explicit availability handling. |
| 11 | Measure and improve pause/resume/all-stop refresh latency | Open. Do not infer improvements from test duration. Separate backend operation/event timing from frontend inspection/source/render timing. An intermittent real-greeter source timeout remains recorded below. |
| 12 | Discoverable keyboard navigation from breakpoint to paused frame | Implemented in help and hints. Alt+2, choose a breakpoint, P, choose a paused hit, Enter; Escape cancels. |
| 13 | One-click topology expansion/collapse and individual signaling | Open. Grouped/flat presentation alone is not folding. Preserve selected scope and explicit session identity when folding. |
| 14 | Show only execution actions supported for selected scope | Implemented. Toolbar filtering uses DDB's typed `execution_action_capabilities`, not only the global action list. Tests cover thread/session/group and no target. |
| 15 | General pretty-printing solution, including greeter C++ string | Open. Existing default printer setup does not establish that greeter `prefix` is pretty-printed. Diagnose debugger formatter registration and evaluation separately; do not hard-code C++ structure parsing in the frontend. |
| 16 | Visible pending/completed/error feedback for controls and other operations | Open. Source-picker loading/error feedback is implemented, but does not satisfy execution-operation feedback. |

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
