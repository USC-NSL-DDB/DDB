# DDB native breakpoint and session controls verification

These changes implement the nine requested interaction refinements on
`codex/tui-next`, against baseline `32b03577`. DDB's existing v2 API remains the
contract. Scoped breakpoint deletion required a backend correctness fix before
the frontend could safely expose individual rule deletion.

## Requirements and delivered behavior

| Requirement | Delivered behavior | Regression evidence |
| --- | --- | --- |
| Keep general stop reasons out of Breakpoints. | Breakpoints shows current matching hit badges, lifetime hit counts, and installation state. Topology marks principal stops with `trigger` and the reason, including signal names. Process-wide stopped siblings and automatically interrupted peers show only `stopped`. | `breakpoint_member_badges_only_report_current_breakpoint_hits`, `pause_reason_belongs_to_topology_not_the_breakpoint_panel`, principal-stop and automatic-pause tests. |
| Separate Session and Group insertion. | The target picker has two tabs. Tab, Left/Right, or a mouse click switches types and clears the previous selection. Only visible targets can be submitted. Empty tabs do not select another type implicitly. | `breakpoint_type_tabs_exclude_hidden_targets_and_clear_cross_type_selection`; terminal creation and deletion workflow. |
| Show pending toolbar actions in place. | An animated spinner replaces the action icon without changing button geometry. A duplicate pending action with the exact same target is not queued again. Other actions and targets remain independent. | `pending_toolbar_action_is_visible_in_place_and_repeated_click_is_not_queued`, feedback identity test. |
| Consolidate toolbar and connection information. | The title is the requested `DDB TU`, with its help hint. Connection and running/stopped totals appear on the bottom Status border, including compact layouts. | Toolbar rendering and responsive-layout tests. |
| Put scope on the border. | Scope appears on the top-right toolbar border, with a preserved `[c]` shortcut and a mouse target. | Toolbar bounds, long scope, and click-dispatch tests. |
| Make Signal discoverable. | `Signal [s]` appears beside Kill when the pane is wide enough. The keyboard shortcut remains available in narrow layouts. | Session-only rendering test and terminal signal workflow. |
| Restore hints after confirmed feedback. | Completed feedback expires after five seconds. Failed/cancelled feedback expires after eight seconds. Pending and unconfirmed outcomes do not expire. Status receipt text clears after five seconds without erasing newer messages. Idle unconfirmed feedback does not keep the animation timer active. | Feedback timeout and receipt-expiry tests. |
| Manage individual breakpoint rules safely. | Parent rows identify Session, Group, or Mixed rules. Children distinguish independent session rules from inherited group members. Delete opens a scope confirmation, with a separate all-rules choice. Inherited members select their group rule. Pending groups remain deletable without installed members. | Backend scoped-target tests, inherited-member and stale-confirmation tests, terminal scoped-deletion workflow. |
| Offer session-only Topology controls. | `T` hides/restores threads without changing inspection or toolbar scope. `C`/Run and `I`/Pause address the selected session. In session-only mode, Signal also addresses that session, including custom signals. Kill retains its identity-checked confirmation. | Deterministic session-scope and custom-signal tests, mock two-session workflow, real GDB/LLDB Run/Pause workflow. |

## Backend scoped deletion correction

The API accepted a deletion target but previously executed a global native
`-break-delete LOGICAL_ID`. Deleting one independent session rule could therefore
remove another session's breakpoint. Completion also emitted a logical deletion
even when a partial deletion should have retained the resource.

Deletion now resolves exact stored rule identities before admitting the operation.
Session targets match independent Session rules, not inherited group members.
Group targets match Group rules. Multiple and SessionSet targets must match every
requested rule. Broadcast explicitly selects every stored rule. Invalid or
partially unmatched targets are rejected before native mutation.

The native deletion path validates all rule IDs before mutating, uses the existing
group/operation locks, and retains unrelated rules. Deleting the last rule removes
the logical breakpoint. Partial deletion returns the updated typed breakpoint
and publishes an upsert instead of a tombstone. Existing partial native failure
reporting remains available. No new transport or compatibility API was added.

The regression failed before the fix because deleting Session A left Session B
with zero native breakpoints instead of one. Tests now check retained logical and
native state for independent sessions, retained rules across groups, and rejection
of a Session selector for an inherited member. The UI terminal regression also
checks that the retained Session rule remains visible after deleting its peer.

## Stop attribution and target safety

`StopReason.thread_id` identifies the principal thread in a process-wide stop.
Topology does not infer a principal from the selected frame or from a sibling's
projected execution state. Several independent principal stops can be displayed.

Automatic peer interrupts are frontend policy. The TUI records the running
thread's prior execution revision when queueing an automatic interrupt. Only a
new interrupt-like stop from that request is suppressed. Its exact observed
revision is retained across duplicate snapshots. Resume, a later revision,
retirement, and backend replacement invalidate that attribution. An independent
breakpoint or SIGUSR1 stop is not suppressed merely because an automatic pause
was pending.

This provenance belongs to the observing frontend. A newly connected frontend
without that history displays the backend's reported interrupt rather than
guessing its origin. The API does not currently encode cross-frontend automatic
pause provenance.

Deletion confirmations capture the backend instance and canonical rule target.
Changed rules or a replacement backend invalidate confirmation instead of
redirecting it. Group confirmation warns that current and future members are
affected. The source `b` shortcut retains its existing unique logical-breakpoint
toggle behavior; individual rule management uses Delete in Breakpoints.

Session Run/Pause resolves the visible selected session separately from toolbar
scope. Group rows do not silently select a member. Custom signal prompts preserve
the target chosen in the catalog and reject a replaced backend. Mouse row targets
carry topology identities, so the new action strip cannot be mistaken for a row.

## Verification results

| Check | Final result |
| --- | --- |
| Backend targeting integration suite | 10 passed. |
| Backend unit suite | 352 passed; 2 existing ignored tests. |
| Frontend unit suite | 372 passed. |
| Non-ignored terminal helper tests | 4 passed. |
| Complete opt-in terminal suite | 38 passed, including all five greeter workflows, scoped deletion, distributed caller memory, and real GDB/LLDB session Run/Pause. |
| Existing helloworld `.ddb/ddb.yaml` | One additional managed real greeter workflow passed. Its SHA-256 was unchanged before and after verification. |
| All-target, all-feature Clippy for both projects | Passed with warnings denied and `--no-deps`. |
| Rust formatting and patch whitespace | Passed. |
| Discovery listener and test-owned processes | Port 10101 free; no remaining broker, DDB, TUI, greeter, or loop-fixture process. |

The real tests used the instrumented applications under
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld`. The additional managed
run used that project's `.ddb/ddb.yaml` without changing it, including its native
printer initialization and inline source roots.

Earlier terminal runs exposed stale title/badge expectations and synchronization
bugs in the tests. Readiness now checks an actual picker choice instead of a
Session label behind a loading dialog. Greeter hit navigation waits for the
server's own row instead of clicking a client's older badge. Controls wait for
the current receipt state before repeating the same action. The mock stops itself
25 ms after Continue, so sustained Run/Pause verification uses real GDB and LLDB
instead of trying to observe that transient state by API polling. No readiness
timeout was increased.

After the user quit their running TUI, the real workflows used the available
discovery port without terminating that session or a user application. One
failed test aborted during cleanup and left its own broker alive. Its PID and
temporary config were verified before stopping it. The hit-row assertion now
releases the captured-frame mutex before asserting, avoiding the poisoned-lock
cleanup failure.

## Reproducing verification

Build the sibling DDB binary and real fixtures as described in the
[TUI README](../../ddb-tui/README.md#end-to-end-tests). Run the fixed-discovery
greeter workflows serially, with no other debugging broker on their configured
port.

```bash
cargo test --manifest-path ddb/Cargo.toml --locked --offline \
  -p ddb --test api_v2_targeting
cargo test --manifest-path ddb/Cargo.toml --locked --offline -p ddb --lib
cargo clippy --manifest-path ddb/Cargo.toml --locked --offline \
  -p ddb --all-targets --all-features --no-deps -- -D warnings

cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --all-targets --all-features --no-deps -- -D warnings
cargo fmt --manifest-path ddb/Cargo.toml --all -- --check
cargo fmt --manifest-path ddb-tui/Cargo.toml -- --check
git diff --check

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
DDB_E2E_BIN=/absolute/path/to/DDB/ddb/target/debug/ddb \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock -- --ignored --test-threads=1

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
DDB_GREETER_PROJECT_CONFIG=/absolute/path/to/helloworld/.ddb/ddb.yaml \
DDB_E2E_BIN=/absolute/path/to/DDB/ddb/target/debug/ddb \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock greeter_auto_attach_single_config_managed_terminal_workflow \
  -- --ignored --exact --test-threads=1
```

The real tests refuse an occupied discovery port and preserve its configuration
file. They own their broker, debugger backend, and greeter applications. Test
cleanup does not terminate an existing user session or application.

## Auditable commit boundaries

The worktree was clean at the baseline. Each commit stages an explicit file
allowlist and excludes unrelated files and generated artifacts.

1. `22cd87fd`, `fix(api): honor breakpoint rule scope during deletion`.
   Backend target validation, unified native rule deletion, scoped integration
   regressions, and the v2 deletion semantics guide.
2. `a863df79`, `feat(tui): refine native breakpoint and session controls`.
   Frontend models, event and command dispatch, rendering, searchable help, and
   deterministic interaction regressions. These share the existing App state
   and control paths.
3. `test(tui): verify scoped controls and real debugger workflows`.
   Terminal regressions, current user documentation, and this audit.

Review the complete change with
`git diff 32b03577..HEAD -- ddb/core ddb-tui ddb/docs`.
