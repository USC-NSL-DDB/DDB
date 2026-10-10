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
a supported shorthand. The no-argument form still connects to
`http://127.0.0.1:5000` for existing development workflows.

Useful managed options are:

```text
--ddb-path PATH          choose the backend executable explicitly
--startup-timeout SECS   bound startup and readiness waiting (default: 20)
--backend-log PATH       retain backend stdout/stderr at a new private path
--config PATH            explicit alternative to the positional config
--values-formatting MODE pretty-printers (default), raw, or disabled
```

Value presentation applies to locals and watches. `pretty-printers` enables
installed GDB visualizers through DDB's typed `ConfigureDebugger` API when a
session becomes ready or stopped, including auto-attached sessions. LLDB uses
its existing formatters. `raw` keeps canonical child expansion without enabling
GDB visualizers. `disabled` hides child expansion while retaining scope loading,
values, and editable variable identities. These last two modes do not disable
formatters already enabled by another frontend or debugger configuration.

The default also enables GDB visualizers when connecting to an externally owned
DDB backend. Use `--values-formatting raw` to avoid that configuration change.
Setup is attempted once per eligible session and backend instance; failures are
shown in status and the diagnostic timeline, without repeated automatic retries.

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

The TUI requires the current API v2 and uses the typed Rust SDK for every
backend request. There is no v1 fallback or alternate protocol mode. Upgrade
the backend and frontend together when the API contract changes.

### Debugger session settings

Pass `--debugger-settings /absolute/path/settings.yaml` to apply backend settings
to each ready GDB/LLDB session, including sessions discovered by automatic attach.
The file is explicit opt-in and is read before starting the managed backend.

```yaml
source_mappings:
  - from: /build/checkout
    to: /srv/source/checkout
gdb:
  commands:
    - set print elements 33
lldb:
  commands:
    - settings set target.max-children-count 33
```

Setup enables GDB pretty printers when requested by `--values-formatting`, applies
source mappings in file order through `ConfigureDebugger`, then runs the matching
backend's commands through `ExecuteRawCommand` in native CLI syntax. Mapping
paths refer to the debugger host; they are not local TUI source-workspace paths
and are not resolved relative to this YAML file. Prefer absolute paths.

Only use trusted settings files. Startup commands can change or resume the
debuggee and have the same authority as the native debugger console. A failed
operation stops the remaining setup; already applied changes are not rolled back.
Errors are reported in the TUI; startup command output is collected in Output,
which you can show with `O`. A refresh or reconnect to the
same known session does not repeat setup, including failed attempts. New sessions
and a new DDB server instance receive their own setup. Restarting the TUI applies
setup again. To retry deliberately without restarting, use the native console.
Setup waits for Ready/Stopped sessions and does not interrupt running sessions.
Files are limited to 64 KiB, 128 mappings, and 128 commands per backend; unknown
keys, empty commands and control characters are rejected.

## Interaction

| Input | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move panel focus |
| `j` / `k`, Up/Down, mouse wheel | Navigate the focused panel |
| Page Up / Page Down | Move by the focused pane's visible page size |
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
| `F` | Add a function/symbol breakpoint, then choose session/group targets |
| `d` | Refresh the DDB distributed stack (normally refreshed automatically) |
| `Enter` in Variables | Load/collapse a scope or expand/collapse variable children |
| `e` | Evaluate an expression |
| `w` | Add a retained watch expression in Variables |
| `Delete` in Variables | Remove the selected watch expression |
| `m` | Read memory (`ADDRESS` or `ADDRESS ; BYTE_COUNT`) |
| `M` in Variables | Read 256 bytes at the selected variable/watch's DDB-provided address |
| `g` | Go to a source line (loads another source window when needed) |
| `Left` / `Right` in Source | Scroll long source lines horizontally, keeping the gutter fixed |
| `Home` in Source | Return to the left edge of the source text |
| `o` | Open the source explorer at local and DDB-exported roots |
| `Ctrl+P` | Search debugger-known files across DDB sessions |
| `f` in Source | Resume following the selected thread's execution location |
| `Alt+Left` / `Alt+Right` | Navigate source history without changing debugger selection |
| `J` | Open the execution-jump prompt |
| `s` | Send a signal to the selected thread |
| `a` / `Enter` in Extensions | Invoke an action declared by the active DDB extension |
| `Left` / `Right` in Extensions | Select an extension action without invoking it |
| `Up` / `Down`, Page Up/Down in Extensions | Scroll extension data, even when actions are available |
| `:` | Run a native GDB/LLDB command in the selected frame |
| `r` | Refresh all state |
| `?` | Toggle the in-app help overlay |
| `q` / `Ctrl+C` | Quit |

### Logpoints and value inspection

To configure a logpoint, create a normal DDB breakpoint, select its row in
Breakpoints, and press `L`. Enter a message such as `counter={counter}`.
`{{` and `}}` produce literal braces. The setting applies to the logical
breakpoint, including its group/session members. Press `L` and submit an empty
message to restore an ordinary stopping breakpoint.

Logpoints write to Output and continue their originating thread only after
successful evaluation and stop revalidation. Failures leave the thread paused.
Explicit execution, evaluation, assignment, or logpoint configuration cancels
older automatic logpoint work. Messages are limited to 64 KiB and 128
expressions. Logpoint metadata belongs to this TUI process, is not persisted,
and is discarded when the DDB server instance changes. Other frontends still
see the underlying DDB breakpoint. Stop validation and Continue are separate
API calls, not an atomic backend stop precondition.

In Variables, select a value and press `=` to assign it using the debugger's
value syntax. Scope headings cannot be edited. The request uses the variable's
opaque identity and owning frame, not its display name. A changed inspection
context cancels submission; backend read-only/stale-handle errors are reported.

In Registers, `=` edits the selected available register through DDB's typed
evaluation API, using the selected frame and its owning thread. Enter a value
in debugger expression syntax. Register and variable views refresh after a
successful assignment. As in the VSCode adapter, register assignment is a
mutating REPL evaluation; expressions may have side effects.

Watches refresh for the selected stopped frame, including distributed caller
frames and after variable assignment. Enter expands retained values through
DDB's variable API. Errors appear on the watch row. Expressions survive resume
and reconnect within this TUI process, but old values and handles do not.
Watches are not saved across TUI restarts. Expressions use debugger syntax and
may have side effects; prefer expressions that only read state.

The `:` console accepts the selected session's native debugger CLI, such as
`print counter` in GDB or `frame variable` in LLDB. It uses the selected stack
frame's canonical ownership, including distributed callers. It does not accept
GDB/MI as a compatibility command language. Use `b`/`B` for DDB-managed
breakpoints with session or group targeting. `F` accepts a backend-supported
function or symbol expression without requiring an open source file. Its target
picker requires an explicit selection, including broadcast or inherited groups.

In Breakpoints, press Enter or click a row to open its source. Up/Down visits
the logical breakpoint and its session-specific sites. If a logical breakpoint
has multiple sites, select a child row to choose the source owner explicitly.
Opening breakpoint source does not change the execution target.

Source horizontal scrolling also accepts horizontal mouse-wheel events. It
preserves the source cursor and execution-following mode. Navigation history
restores each file's horizontal offset; resizing clamps it to the content width.
Vertical movement keeps the viewport stable until the cursor reaches an edge.
Loading another source window preserves that edge instead of recentering the
cursor; pending loads use the current pane height if the terminal is resized.

`?` opens scrollable help. Use Up/Down, `j`/`k`, Page Up/Down, Home/End,
or the mouse wheel; `Esc` or `?` closes it. Help wraps to the terminal width
and blocks clicks on debugger controls underneath it.

Click `[Expand]` on a pane's bottom border or press `z` to expand the focused
pane; click `[Restore]`, press `z` again, or press `Esc` to restore
the layout. `Tab` still changes the focused pane while expanded. The diagnostic
timeline is hidden by default. Press `t` to show and focus it, or hide it again.
Timeline events continue to be collected while the pane is hidden.
Up/Down, Page Up/Down, and the mouse wheel scroll wrapped timeline rows,
including within a single long event. New events return the view to the tail.
Registers have their own focus and selection: click the register pane or use
`Tab`, then use arrows, Page Up/Down, or the mouse wheel to browse rows. Left/Right
scroll long register values horizontally; `z` expands the register pane alone.

`M` in Variables reads 256 bytes at the selected variable or watch's API-provided
address, using the owning distributed frame. Values without an address report
that limitation; the TUI does not interpret their display text as an address.
LLDB supplies native storage addresses for addressable values. GDB address
metadata is still pending; use `m` to enter an address for those values. Register-only,
unavailable, and other values without native storage can have no address.

`m` reads memory and focuses its pane. Memory has independent row selection;
Up/Down, Page Up/Down, and the mouse wheel reach every returned row. Left/Right
reveals the hex and ASCII columns without wrapping them. Use `z` or the pane's
Expand button for a full-width view. New reads reset the view; changing frames
or resuming execution clears stale memory. Distributed-frame reads target that
frame's owning thread, not the thread where the distributed stack began.

In Topology, Breakpoints, Stack, Variables, Registers, and Memory, Left/Right or a horizontal mouse wheel reveals clipped
names and values. Home returns to the start. Each pane remembers its position;
resizing clamps scrolling to the available content. Horizontal navigation does
not select another frame or issue debugger commands.

Press `O` to show/hide the Output pane. Native console and one-shot evaluation
results open it automatically. Program output is collected there even while it
is hidden; the optional timeline remains for state/operation diagnostics.
Up/Down, PageUp/PageDown and the mouse wheel browse history. Home goes to the
oldest retained line; End resumes following new output. Left/Right or horizontal
wheel scrolling reveals long lines. Scrolling back prevents new output from
moving your view. The buffer retains 1,000 lines of at most 4,096 characters each;
evicted-line counts and line truncation are shown explicitly. Terminal control
characters are displayed safely rather than executed.
Byte-backed output is decoded as UTF-8, replacing invalid byte sequences with
replacement characters, as in the VS Code adapter. This is a text view, not a
lossless binary export.
Press `F8` from any pane, or click `Frame F8` in the toolbar, to reveal the
selected stack frame's source without changing the execution target. Unlike
`f` in Source, this reveals the selected frame rather than enabling execution
following. Missing source locations and non-source boundary rows report why
they cannot be opened.

### Source explorer

`o` or the source panel's Open button lists explicitly configured roots, marked
`[Local]` or `[DDB]` by their host. To browse directories on the DDB host, add
exports to the DDB startup configuration, including with `ddb tui debug.yaml`:

```yaml
SourceRoots:
  - name: application
    path: ./checkout
    debugger_path: /build/application
    session_tags: [api, worker]
```

`path` is on the DDB host, relative to the configuration file. The optional
`debugger_path` maps a build prefix to that export; optional `session_tags`
select the sessions using that mapping. These tags do not restrict readers'
permissions. See [source workspaces](../ddb/docs/api/source-workspaces.md).

To browse a checkout on the **TUI host**, create a separate `workspace.yaml`:

```yaml
roots:
  - name: api-checkout
    path: ./checkout
    debugger_path: /build/application
    session_tags: [api]
  - name: notes
    path: ./notes
```

```bash
ddb tui --source-workspace ./workspace.yaml ./debug.yaml
# Or connect to an existing backend:
ddb-tui --source-workspace ./workspace.yaml connect --api http://127.0.0.1:5000
```

Local `path` values are relative to `workspace.yaml`, never the backend config
or debugger working directory. Nothing is implicitly exported from the current
directory. `debugger_path` is an optional absolute slash-separated build prefix;
without it, files can be read but cannot create source breakpoints. Optional
`session_tags` select configured DDB tags through the canonical `Session.tag`
field, not session labels. Use separate roots with distinct tags for different
distributed checkouts of the same build path.

Explicit local mappings take precedence over DDB content reads. The longest
matching path prefix wins; a tagged mapping wins over an untagged peer; ties are
errors. A missing or denied mapped file is an error, never a reason to silently
switch to DDB content. Opening an untagged mapped root manually requires choosing
breakpoint targets; opening it through an owned debugger descriptor retains that
descriptor's owner. Local configuration is not sent to DDB and does not modify
debugger-host source mappings.

Browse directories with arrows and Enter, or click a row. Left/right-click goes
to the parent, Tab switches between roots and debugger files, F5 refreshes, and
Ctrl-H shows dotfiles. Type to search the debugger catalog or filter the current
directory page; Ctrl-U clears the query. Previous/next rows navigate bounded
100-entry pages. For a symlink, Enter/click tries a file and Right tries a
directory. Both providers reject links that escape their configured root.
Esc or Close exits.

The debugger catalog identifies session owners and keeps unavailable sources
visible with their error reason. Workspace folders also contain files absent
from debug symbols and work without any selected frame or running session.
Opening a file does not change debugger selection, evaluation context, or the
execution target. It disables following until `f` is pressed in Source.
Alt-Left/Right restores document history and cursor positions.

The source title identifies the document's owners. Execution and breakpoint
markers require both a matching owner and the exact declared debugger path;
equal filenames or path suffixes do not establish a relationship. A mapped
workspace file uses `debugger_path` for breakpoints, never its display path.
Without that mapping, source breakpoint placement is rejected.

A backend-instance change invalidates DDB source references and remote history.
Local documents, cursor positions, and local history remain available, including
after disconnection. Old frame-owner hints are discarded; tagged roots bind to
the new session projection. Source responses must match their expected server
instance even before a replacement state snapshot arrives.

Local reads share bounded caches across roots: at most 32 roots, 2 MiB per file,
240 lines per read, and 100 displayed directory entries per page. Directory
enumeration is nonrecursive and bounded at 100,000 entries/16 MiB. Content and
directory caches each hold at most 32 entries/32 MiB. Four filesystem jobs may
run at once, including jobs whose UI request was cancelled. First-page refresh
invalidates stale pagination; cache hits recheck filesystem access. Root discovery
waits at most two seconds for DDB when local roots are available, then shows
those roots with a warning and an explicit refresh action.

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

The picker preselects a document's single owner, independent of the selected
thread. Unbound and multi-owner documents require an explicit target choice.
`b` deletes only a unique matching logical breakpoint. If several match, it
focuses the breakpoint manager so you can select which one to delete. Session,
group, broadcast, and multi-target scopes remain available explicitly.

The `B` prompt accepts `-t`/`--temporary`, `-h`/`--hardware`, `--ignore-count N`, and a condition
introduced by `if`, `-c`, or `--condition`. For example:

```text
-t if request.id == 42
--hardware --condition ptr != 0
--ignore-count 3 -t
```

`--ignore-count N` skips the next N hits. Without `-t`, the breakpoint keeps
stopping afterward; with `-t`, it stops once and is removed. Counts must fit an
unsigned 64-bit integer. Put options before the condition expression.
The `F` function prompt accepts the same options after a space-delimited `--`,
for example `Greeter::SayHello -- --ignore-count 3 -t`. Function names may contain
spaces, including C++ operator names and signatures.

The prompt and all breakpoint mutations are capability-gated; unsupported
options are rejected before any request is sent to DDB.

Breakpoint rows show `▶ N paused` for threads currently stopped at that
breakpoint. This is separate from the cumulative `hits:N` counter. Automatically
paused all-stop peers are not counted. Member rows show current hits only when
DDB identifies that member or there is a single member in the hitting session.
Markers disappear when the thread resumes or the backend disconnects.
Press `P` in Breakpoints, or right-click a breakpoint row, to choose a currently
paused hitting thread. Enter or a click loads that thread's stack, source and
variables. Esc or Cancel closes the picker. The picker validates the stop revision
before navigating and rejects hits that resumed or changed while it was open.
This navigation does not change the execution-control target.

The variables panel preserves the scopes returned by DDB. Non-expensive scopes
load automatically; expensive scopes (such as globals and file statics) load
only when expanded. Scope and variable handles are valid for the current stop
and are discarded when the inspection changes. Evaluation and explicit frame
reveal use the owning thread of the selected distributed frame, not the thread
where the backtrace began. Manual file browsing does not change that context.
The expression prompt uses the API's REPL context; it does
not create retained watch values.

Memory reads default to 128 bytes. Add `; BYTE_COUNT` to choose a bounded
size, for example `$sp + 16 ; 256`.

The SDK hydrates a bounded snapshot, applies revisioned state events, and
replays from its acknowledged cursor after a disconnect. Execution-state
revisions refresh stop-scoped inspection even when a fast step has no visible
running transition or stops again on the same line. Output uses a
separate stream so a slow console cannot starve stopped-state delivery. The UI
shows replay gaps explicitly. A low-frequency refresh remains as recovery for
backend availability changes.

## Layout

- Left: DDB group/session/thread topology and aggregate distributed breakpoints
- Center: source view and the DDB distributed call stack
- Right: named variable scopes and lazy children, registers, memory, extensions, and DDB timeline
- Top: capability-gated controls, current execution scope, and connection state
- Bottom: status, command palette, and contextual key hints

DDB-host sources are read through the typed API. Explicit local checkouts are
read on the TUI host. The frontend and backend need not share a filesystem.

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
a user would. It includes deterministic mock and late-backend recovery cases,
plus real GDB and LLDB sessions. Backend assertions use the same typed v2 SDK. The tests
validate rendered terminal state with a VT100 screen model and verify
backend-visible distributed breakpoint changes, group/session/thread topology
and execution scopes, event-driven stops, distributed stack boundaries and
inspectable frames, lazy variables/registers, evaluation, memory reads,
capability-gated controls, exact post-step execution-line markers, independent
source-cursor movement, mouse input, and terminal restoration. Real GDB/LLDB
cases also verify startup command output, installed backend source mappings,
register assignment/restoration, and logpoints continuing to an ordinary stop.

Managed PTY cases additionally cover direct and dispatched one-command startup,
two-session partial readiness, authenticated token redaction and early unlinking,
startup timeout/early exit, backend crash, SIGTERM, terminal SIGHUP, GDB/LLDB
launch kill-on-quit, and GDB attach detach-on-quit.

From `ddb-tui/`:

```bash
cargo build -p ddb --manifest-path ../ddb/Cargo.toml
cargo build --manifest-path ../ddb/core/tests/fixtures/real_loop/Cargo.toml
cargo test --test e2e_mock -- --ignored --skip greeter_auto_attach --test-threads=1
```

The PTY suite currently targets Unix and requires `script`, GDB, and LLDB.
`DDB_E2E_BIN` may be set to test a different DDB executable.

The opt-in auto-attach greeter regression additionally requires an instrumented
gRPC helloworld checkout, `ddb_runapp`, Mosquitto, and an existing
`/tmp/ddb/service_discovery/config` pointing at a free loopback broker port with
topic `service_discovery/report`. It refuses an occupied port and does not rewrite
the discovery file. It owns its broker and applications; do not run another
discovery session while this test runs.

```bash
DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
  cargo test --test e2e_mock greeter_auto_attach_all_stop_terminal_workflow \
  -- --ignored --test-threads=1
```
