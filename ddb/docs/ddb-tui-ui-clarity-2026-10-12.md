# DDB TUI control and breakpoint presentation audit

This change follows baseline `77dc9265` on `codex/tui-next`. It addresses the
control layout, the ambiguous `Frame F8` caption, and breakpoint-hit prefixes.
It also fixes a pause-focus race found during real helloworld verification.
DDB's API, configuration contract, dependencies, and execution defaults do not
change.

## Requirements and decisions

| Requirement | Implementation | Regression evidence |
| --- | --- | --- |
| Make each action and mouse target identifiable. | Shaded, bracketed buttons contain their icon, full action name, and shortcut. Execution and navigation occupy separate groups. Whole buttons wrap; gaps and status text are not clickable. | Complete labels and per-cell hit testing at 140, 100, 80, and 45 columns; Unicode scope labels; tiny-terminal bounds and capability filtering. |
| Explain F8 without implying a frame number or execution jump. | `Show frame source F8` opens or retries the selected stack frame's file at that frame's line. It never resumes, steps, or selects another frame. | Rendered caption assertion, existing selected-frame navigation tests, and a terminal mouse click after browsing away that restores source without moving the backend frame. |
| Separate breakpoint identity, current pauses, and history. | Red enabled `●` or muted disabled `○` remains before the location or symbol. A right-aligned amber `[N paused]` badge shows live pauses. Session members have their own badges; muted `total hits:N` is historical. | Styled badge geometry, resume clearing, member ownership, retained history, horizontal scrolling, and real GDB/LLDB and greeter hits. |

At a wide terminal the toolbar reads:

```text
[ ▶ Continue F5 ] [ ⏸ Pause F6 ] [ ↷ Step over F10 ] [ ↓ Step in F11 ] [ ↑ Step out Shift+F11 ]
[ Show frame source F8 ] [ ↻ Reload DDB stack d ] [ Refresh r ] [ Scope: Auto c ]
● connected · … running · … paused · Auto: Continue/Pause all; Step selected frame
```

These are the captions exercised in the real terminal workflows, not additional
actions. Unsupported actions remain hidden. A single private toolbar layout
computes terminal-cell widths, wrapping, drawing rectangles, and mouse targets.
It replaces the former fixed-width toolbar implementation. No second coordinate
table or configurable button framework was introduced.

Breakpoint text scrolls horizontally while the live badge remains pinned. The
reserved badge width is included in the scroll limit so the end of a long label
or diagnostic remains reachable. Clipped labels use a grapheme-safe ellipsis
without discarding their colors. Extremely narrow panes prioritize label text;
expand the pane to see the badge. The existing keyboard controls remain usable
when a terminal cannot fit a whole toolbar button. Below 19 terminal rows, the
existing mini-header layout applies.

The breakpoint footer separates `Paused [H]`, `Files [v]` or `Flat [v]`, and
`Expand`. Moving grouping out of the header prevents it from covering the title
and current-pause count. Function breakpoints no longer invent a source `:0`.
Canonical live-hit matching, parent/member selection, and hit navigation are
unchanged; automatically paused peers do not become breakpoint hits.

## Real-app bug found and fixed

The managed greeter workflow intermittently retained the server main thread
after its RPC breakpoint hit, showing `syscall` without source instead of
`GreeterServiceImpl::SayHello`. The pause badge correctly showed the RPC hit,
so the failure was not a missing breakpoint or a badge-rendering problem.

A tagged, temporary stop-order trace showed a snapshot overtaking the old
owner's running event. Snapshot reconciliation cleared the all-stop cause but
left `pause_focus` intact. By the next process-wide stop, that old thread was
stopped again, so the first-pause lock incorrectly appeared valid.

`App::apply_threads` now releases the focus lock when its owner is no longer
present and stopped. The deterministic regression
`a_running_snapshot_releases_pause_focus_before_a_later_process_wide_stop`
fails before the fix and passes after it. It applies a running snapshot without
a running event, then a process-wide stop owned by a different thread in the
same session, and asserts automatic selection of that new owner.

Waiting for `0 running` did not reliably solve the failure and was removed.
The original real workflow still checks RPC selection before any manual hit
navigation. No sleep, readiness timeout increase, or source-selection workaround
was retained. All temporary tracing was removed.

## Verification

| Check | Result |
| --- | --- |
| TUI unit suite | 355 passed |
| Non-ignored terminal helper tests | 4 passed |
| Complete ignored terminal suite | 36 passed, including all five greeter workflows and real GDB/LLDB |
| Existing helloworld `.ddb/ddb.yaml`, unchanged | 1 additional real workflow passed |
| All-target, all-feature Clippy with warnings denied | Passed |
| Rust formatting and patch whitespace | Passed |
| Verification cleanup | Discovery port 10101 free; no remaining debugger, broker, or greeter process |

The terminal helpers now locate the rendered topology header, toolbar buttons,
and source rows instead of assuming the previous toolbar height. Variable
selection expands its pane when the requested row cannot fit inline. These
changes preserve the backend and navigation assertions. Helper regressions
cover adjacent pane borders, source markers, and a wrapped toolbar.

The new caption and pause-badge assertions failed before the redesign.
Intermediate suite runs also caught obsolete fixed-row assumptions, an expanded
variable lookup still restricted to the old right-hand columns, and a footer
test fixture that had not focused Breakpoints in compact mode. Those test issues
were corrected before final verification.

Reproduce from the repository root with DDB, GDB/LLDB, and the instrumented
greeters built as described in the [README](../../ddb-tui/README.md#end-to-end-tests):

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

The project-config run uses
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld/.ddb/ddb.yaml`.
Tests refuse an occupied discovery listener, preserve and restore its discovery
file, and clean up only their owned brokers and applications. The user quit
their TUI before verification; no existing user process was terminated by the
test harness.

## Commit boundaries

Each commit uses explicit staging paths from the clean baseline:

1. `230b23ef`, `fix(tui): release pause focus when snapshots observe resume`.
   The snapshot reconciliation fix and its deterministic stop-order regression.
2. `b3b1d2c7`, `refactor(tui): clarify toolbar actions and breakpoint pause badges`.
   The shared toolbar layout, breakpoint rendering, text clipping, help, and
   focused rendering regressions.
3. `test(tui): verify redesigned controls and document navigation`.
   Updated terminal workflows, current user documentation, and this audit.

Review the complete change with `git diff 77dc9265..HEAD -- ddb-tui ddb/docs`.
