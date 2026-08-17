# ddb-tui

`ddb-tui` is a mouse- and keyboard-friendly terminal debugger frontend for
DDB API v2. It consumes the public `ddb-api-client` crate over the stable
HTTP/ProtoJSON binding and its independent state/output NDJSON streams. It is a
separate Rust project so the UI can iterate and ship independently from the
backend.

## Run

With paired `ddb` and `ddb-tui` binaries, the recommended configured workflow is:

```bash
ddb tui ./debug.yaml
```

The direct frontend form is equivalent:

```bash
ddb-tui ./debug.yaml
```

Both forms start and supervise a private headless DDB backend. They use an
authenticated OS-assigned loopback port, wait for an atomic readiness report,
negotiate API v2, and shut down only that owned backend when the TUI exits. A
distributed configuration uses the same command; remote targets do not require
manual frontend/backend orchestration.

Launch or attach without writing YAML:

```bash
ddb tui launch --backend gdb -- ./target/debug/app --flag "value with spaces"
ddb tui attach --backend lldb --pid 12345
```

Launch stops at entry by default and kills its launched debuggee on exit. Pass
`--no-stop-at-entry` to start immediately. Attach detaches on exit and leaves the
original process alive. `--` is mandatory before a launch command so arguments
are preserved as an exact vector and are never interpreted by a shell.

Use external connect mode only when a service manager, container, shared team
service, or automation owns DDB:

```bash
ddb-tui connect --api https://debug.example.com --token "$DDB_API_TOKEN"
```

Quitting connect mode never shuts down that backend. `ddb-tui --api URL` remains
a supported compatibility alias. The no-argument form still connects to
`http://127.0.0.1:5000` for existing development workflows.

Useful managed options are:

```text
--ddb-path PATH          choose the backend executable explicitly
--startup-timeout SECS   bound startup and readiness waiting (default: 20)
--backend-log PATH       retain backend stdout/stderr at a new private path
--config PATH            explicit alternative to the positional config
```

Managed credentials are independent random control/admin tokens stored only in a
private temporary file. Token values are never placed in argv, child environment,
startup reports, logs, or status text. Managed mode always suppresses configured
remote API exposure. Backend failure retains the operational log and shows its
path plus a bounded tail; ordinary successful runs remove the temporary log
unless `--backend-log` was supplied.

The [integrated user guide](../ddb/docs/ddb-tui-user-guide.md) covers distributed
configuration, authentication, failure recovery, upgrade/rollback, and uninstall.

For source-tree development, the projects use separate target directories:

```bash
cargo build -p ddb --manifest-path ../ddb/Cargo.toml
cargo run -- \
  --ddb-path ../ddb/target/debug/ddb \
  ../ddb/examples/managed/mock.yaml
```

API v2 is the default and no silent downgrade occurs. During a controlled
migration from an older externally owned DDB deployment only, opt into:

```bash
ddb-tui connect \
  --api http://127.0.0.1:5000 \
  --api-version v1-fallback
```

Fallback still negotiates v2 first and occurs only when the v2 discovery route
is explicitly absent. Authentication, permission, connectivity, malformed
protocol, or managed-startup failures never cause a downgrade. V1 lacks
cursor-based state/output replay, so reconnects force a refresh and warn that
console output may have been lost. New frontend development should use the typed
v2 SDK.

## Interaction

| Input | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move panel focus |
| Arrow keys / mouse wheel | Navigate the focused panel |
| `Enter` / click | Select a DDB group/session/thread target, inspect a thread/frame, expand a variable, or activate a row |
| `F5` | Continue |
| `F6` | Interrupt/pause |
| `F10` | Step over |
| `F11` / `Shift+F11` | Step in / step out |
| `c` | Cycle execution scope: thread → session → group → all eligible sessions |
| `b` | Toggle a breakpoint at the source cursor |
| `B` | Create a conditional, temporary, or hardware breakpoint |
| `Space` in the target picker | Select one or more session/group targets, or server-resolved broadcast |
| `x` / `Space` | Enable or disable the selected breakpoint |
| `Delete` | Remove the selected breakpoint |
| `d` | Refresh the DDB distributed stack (normally refreshed automatically) |
| `Enter` in Variables | Expand or collapse lazy variable children |
| `e` | Evaluate an expression |
| `m` | Read memory (`ADDRESS` or `ADDRESS ; BYTE_COUNT`) |
| `g` | Go to a source line (loads another source window when needed) |
| `j` | Jump execution to a debugger location |
| `s` | Send a signal to the selected thread |
| `a` / `Enter` in Extensions | Invoke an action declared by the active DDB extension |
| `:` | Open the raw DDB command palette |
| `r` | Refresh all state |
| `?` | Toggle the in-app help overlay |
| `q` / `Ctrl+C` | Quit |

The distributed call stack is the primary stack view. Selecting a stopped thread
requests DDB's cross-session backtrace automatically; `d` is only an explicit
refresh. Ordinary rows keep their owning session/thread and use normal,
inspectable v2 frame IDs. DDB boundary rows remain visible but intentionally
cannot be inspected. A truncated result keeps `TRUNCATED` in the panel title and
reports the server's reason in status and timeline.

The topology panel is grouped as DDB group → session → thread. Selecting a group
or session changes the execution target without stealing the thread being
inspected; selecting a thread changes both. Controls are sent to the displayed
scope only when `GetCapabilities` advertises that action/scope combination.

Breakpoint creation always opens a DDB target picker. It supports a
server-resolved broadcast, inheritable group targets, individual sessions, and
explicit multi-target selection as advertised. A selected group absorbs its
redundant member-session selections. The manager preserves aggregate and
per-session sub-breakpoint status, including pending/verification messages.

The `B` prompt accepts `-t`/`--temporary`, `-h`/`--hardware`, and a condition
introduced by `if`, `-c`, or `--condition`. For example:

```text
-t if request.id == 42
--hardware --condition ptr != 0
```

The prompt and all breakpoint mutations are capability-gated; unsupported
options are rejected before any request is sent to DDB.

Memory reads default to 128 bytes. Add `; BYTE_COUNT` to choose a bounded
size, for example `$sp + 16 ; 256`.

The SDK hydrates a bounded snapshot, applies revisioned state events, and
replays from its acknowledged cursor after a disconnect. Output uses a
separate stream so a slow console cannot starve stopped-state delivery. The UI
shows replay gaps explicitly. A low-frequency refresh remains as recovery for
backend availability changes.

## Layout

- Left: DDB group/session/thread topology and aggregate distributed breakpoints
- Center: source view and the DDB distributed call stack
- Right: lazy locals/arguments, registers, memory, extensions, and DDB timeline
- Top: capability-gated controls, current execution scope, and connection state
- Bottom: status, command palette, and contextual key hints

Source content is read through DDB, so the TUI also works when it is running on
a different host from the backend (provided the API endpoint is reachable).

The source gutter keeps three locations distinct: `▶` is the stopped
execution location from stack frame 0, `▸` is the source cursor used for
navigation and breakpoint commands, and `●` is a breakpoint. The cursor
follows new execution stops until you move or click it; after explicit source
navigation it stays pinned while execution moves independently.

The TUI discovers its controls and optional panels through the typed v2
`GetCapabilities` method. Framework-specific data is rendered only when the
active DDB framework advertises an extension descriptor and matching state.
For example, proclet ownership appears for migration-capable frameworks, but
is absent from the default debugger UI. New table-shaped framework panels do
not require framework-specific TUI code.

The layout switches to a single focused panel in smaller terminals. Mouse
clicks focus panels and controls; keyboard access remains available for every
operation.

## Verification

The [DDB-native readiness ledger](../ddb/docs/ddb-native-tui-readiness-2026-08-16.md)
maps the VS Code prototype review, public API decisions, intentional exclusions,
test evidence, and auditable commit sequence.

The normal gate covers API decoding, state projection, stale-response
suppression, event parsing, source identity, mouse hit testing, responsive
rendering, extension discovery, and terminal interaction logic:

```bash
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

### End-to-end tests

The ignored PTY suite starts real DDB processes and drives the compiled TUI as
a user would. It includes deterministic mock, late-backend recovery, and
v1-only migration-proxy cases, plus real GDB and LLDB sessions. The tests
validate rendered terminal state with a VT100 screen model and verify
backend-visible distributed breakpoint changes, group/session/thread topology
and execution scopes, event-driven stops, distributed stack boundaries and
inspectable frames, lazy variables/registers, evaluation, memory reads,
capability-gated controls, exact post-step execution-line markers, independent
source-cursor movement, explicit v1 negotiation, mouse input, and terminal
restoration.

Managed PTY cases additionally cover direct and dispatched one-command startup,
two-session partial readiness, authenticated token redaction and early unlinking,
startup timeout/early exit, backend crash, SIGTERM, terminal SIGHUP, GDB/LLDB
launch kill-on-quit, and GDB attach detach-on-quit.

From `ddb-tui/`:

```bash
cargo build -p ddb --manifest-path ../ddb/Cargo.toml
cargo build --manifest-path ../ddb/core/tests/fixtures/real_loop/Cargo.toml
cargo test --test e2e_mock -- --ignored
```

The PTY suite currently targets Unix and requires `script`, GDB, and LLDB.
`DDB_E2E_BIN` may be set to test a different DDB executable.
