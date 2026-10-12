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

### One project configuration

The project YAML contains DDB settings, persistent debugger setup under
`Conf.Debugger`, and frontend-only preferences under `Tui`:

```yaml
Framework: grpc
ServiceDiscovery:
  Broker:
    hostname: 127.0.0.1
    managed:
      type: mosquitto
Conf:
  on_exit: kill
  Debugger:
    backend: gdb
    gdb:
      commands:
        - set print elements 33
    lldb:
      commands:
        - settings set target.max-children-count 33
Tui:
  source_roots:
    - name: application
      path: ..
      debugger_path: /absolute/build/path/application
  stack_mode: distributed
  source_search: exact
  values_formatting: pretty-printers
```

Run `ddb tui .ddb/ddb.yaml`, then press `o` to browse the local source tree
before any application attaches. `Tui.source_roots` grants read access only to
the frontend. It does not export that directory through DDB's API. Use DDB's
separate `SourceRoots` setting when backend-host files should be exported.

Native setup belongs to DDB and runs once for each debugger session, including
automatic attachments. A frontend refresh or reconnect does not repeat it.
Only the matching backend's command list runs. Commands execute after attach
or symbol loading and before launch execution or framework post-start actions.
Failures abort session bootstrap rather than resuming with partial setup.
Only open trusted project configurations; native commands have debugger-console
authority. DDB does not search the current directory for configuration files.

Optional `Conf.Debugger.source_mappings` is a list of `{from: /build, to: /source}`
entries with absolute paths on the debugger host. `entry_function` installs a
temporary function breakpoint before native commands. It does not rewind a
process that has passed the function. `pretty_printing` enables GDB's MI
visualizers by default; it does not install printer packages or relax GDB trust.

Supported `Tui` fields are `source_roots`, `stack_mode`, `source_search`,
`values_formatting`, `trace_api`, `refresh_ms`, `startup_timeout_secs`,
`watch_file`, `backend_log`, and `ddb_path`. Paths in `Tui` and `SourceRoots`
resolve against their containing configuration file, not the current directory.
`debugger_path` remains an explicit absolute path from the debug information.
Unknown configuration fields are rejected. Backend-only DDB validates `Tui`
but never opens its frontend-local paths.

An optional frontend preferences file, such as [examples/tui.yaml](examples/tui.yaml),
has the same fields without a `Tui` wrapper:

```bash
ddb tui --tui-config ./tui.yaml ./.ddb/ddb.yaml
ddb-tui --tui-config ./tui.yaml connect --api http://127.0.0.1:5000
```

Precedence is built-in defaults, project `Tui`, explicit `--tui-config`, then
explicit command-line options. Root lists replace rather than concatenate.
`--source-workspace` explicitly replaces the resolved local root list.
Use `--trace-api=false` to override a configured `true`. Connect mode rejects
backend-launch settings in frontend preferences; it never changes the external
backend's startup configuration.

Override backend configuration through DDB's canonical parser:

```bash
ddb tui --ddb-set Conf.Debugger.backend=lldb ./.ddb/ddb.yaml
ddb serve --set Conf.Debugger.backend=lldb ./.ddb/ddb.yaml
```

`--ddb-set` accepts a dotted DDB key and a YAML value, not shell code. Repeated
assignments apply in order, with the last value winning. Types and unknown keys
are checked by DDB. Managed mode rejects replacing `Conf`, its `api_*` fields,
and frontend `Tui` settings. Private authentication, transport, readiness, and
lifecycle remain supervisor-owned. `--ddb-arg` remains available for supported
logging, telemetry, and command-worker CLI options.

For instrumented gRPC greeters, copy [examples/helloworld.yaml](examples/helloworld.yaml)
to the application's `.ddb/ddb.yaml`, set its one `debugger_path`, and launch
with `ddb tui .ddb/ddb.yaml`. Start `ddb_runapp ./greeter_server --ddb` and
`ddb_runapp ./greeter_client --ddb` in separate terminals. The example includes
explicit trusted GCC printer loading, so no workspace or printer-profile flags
are needed. Quitting kills debugged applications as configured by `on_exit`.

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
--ddb-arg=ARG            repeat a supported backend option in managed mode
--startup-timeout SECS   bound startup and readiness waiting (default: 20)
--backend-log PATH       retain backend stdout/stderr at a new private path
--config PATH            explicit alternative to the positional config
--values-formatting MODE pretty-printers (default), raw, or disabled
--source-search MODE     exact (default) or fuzzy whole-file source text matching
--trace-api             show sanitized API request metadata in Output
```

In Source, `/` opens text search, Enter finds a matching line, and `n` / `N`
find the next / previous matching line with wraparound. The search covers the
whole bounded file, not only the visible window. In the prompt, Ctrl+f toggles
exact/fuzzy matching, Ctrl+u clears the query, and Escape cancels editing.
Submitting an empty query clears the search. Escape also cancels a pending
search. The Source header's Search button opens the prompt, and the persistent
search bar's mode button toggles matching with the mouse.

Exact matching is case-sensitive contiguous text. Fuzzy matching is a
case-insensitive Unicode lowercase subsequence, with the bounding range
highlighted. Both return one match per line. Use `--source-search fuzzy` to
change the initial mode. Queries preserve spaces and are limited to 1,024
UTF-8 bytes on one line. Searching changes the browsing cursor, never execution.
Remote files use DDB's typed `SearchSource` operation; local workspace files
use the same indexed matcher without requiring a connected backend.

`--trace-api` works in managed and connect modes and is off by default. Press
`O` to view request attempts, canonical RPC names and HTTP statuses, including
startup discovery, retries and stream setup. Traces exclude credentials, headers,
endpoint prefixes, query values, debugger payloads and raw error text. A status
means HTTP headers arrived, not that the debugger operation completed. Stream
contents are never traced. Requests do not wait for trace rendering: the SDK
retains at most 256 records, and the TUI reports dropped records if it falls
behind. Output uses its existing bounded retention and scrolling controls.
If initial negotiation fails before the TUI opens, buffered trace records are
printed to stderr. This option does not enable backend telemetry or persist a
trace file, and is separate from `--backend-log`.

For example, adjust command workers and backend logging without changing the
private managed transport:

```bash
ddb tui --ddb-arg=--command-workers=4 --ddb-arg=--file-level=debug ./debug.yaml
```

Use one `--ddb-arg` per option, with values attached as `--name=value`. Accepted
backend options are `--command-workers`, `--console-level`, `--file-level`,
`--otel-endpoint`, `--otel-level`, `--user-id`, `--session-id`, and the valueless
flags `--console-log` and `--enable-otel`. Telemetry remains disabled unless
explicitly enabled. Quoted values containing spaces remain a single argument;
no shell evaluates them. Do not put credentials in these arguments.

The TUI rejects duplicate options, unknown options, positional arguments,
subcommands, and overrides of its managed transport, authentication or readiness
report. Worker counts must be positive. Individual arguments are limited to
4,096 bytes and cannot contain control characters. Backend-specific values such
as logging filters are validated by DDB. These options apply to managed config,
launch and attach modes, and are rejected in connect mode. Debuggee arguments
after the launch `--` remain untouched.

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

Enabling pretty printing does not install or register a formatter package. For
example, a locally built GDB may use `/usr/local/share/gdb` while the distribution
installs its libstdc++ printers under `/usr/share/gcc/python`. In that case a
`std::string` can still display `{...}` despite successful setup. DDB returns
native formatter summaries, display hints, and expandable children; the TUI
does not decode C++ layouts or invoke target methods to obtain a string.

If `/usr/share/gcc/python` is a trusted formatter installation on the **DDB host**,
opt in with:

```bash
--session-profile /absolute/path/to/ddb-tui/examples/debugger-settings/gdb-libstdcxx.yaml
```

This example explicitly imports the installed GCC printers through the existing
session setup commands. Review it and adjust the installation path for your
distribution. It applies to each GDB session, including later automatic
attachments. It does not change GDB's auto-load safe-path, search your workspace
for Python scripts, download code, or run for LLDB. For other libraries or
languages, use their trusted native GDB/LLDB formatter registration commands in
the same settings file. Startup commands execute code on the debugger host, so
only use settings and formatter packages you trust. See
[GDB variable-object formatting](https://sourceware.org/gdb/current/onlinedocs/gdb.html/GDB_002fMI-Variable-Objects.html)
and [auto-load trust](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Auto_002dloading-safe-path.html).

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

### Optional interactive session profiles

Prefer the project `Conf.Debugger` section for persistent debugger setup.
Use `--session-profile /absolute/path/profile.yaml` only to explicitly apply
interactive settings through the API to ready sessions, including on an
externally owned backend. This replaces the old `--debugger-settings` option;
the old option is not accepted.

The profile applies backend settings
to each ready GDB/LLDB session, including sessions discovered by automatic attach.
The file is explicit opt-in and is read before starting the managed backend.

```yaml
source_mappings:
  - from: /build/checkout
    to: /srv/source/checkout
entry_function: main
gdb:
  commands:
    - set print elements 33
lldb:
  commands:
    - settings set target.max-children-count 33
```

Setup enables GDB pretty printers when requested by `--values-formatting`, applies
source mappings in file order through `ConfigureDebugger`, installs the optional
`entry_function` as an enabled temporary function breakpoint through
`CreateBreakpoint`, then runs the matching
backend's commands through `ExecuteRawCommand` in native CLI syntax. Mapping
paths refer to the debugger host; they are not local TUI source-workspace paths
and are not resolved relative to this YAML file. Prefer absolute paths.

In this optional API profile, `entry_function` applies once to each ready/stopped GDB or LLDB session, including
later automatic attachments. It does not resume execution or rewind a process
that has already passed the function. Omit it to leave entry breakpoints alone.
Use a function present in each target program, or use the interactive function
breakpoint picker for per-target choices. Installation requires advertised
function and temporary breakpoint support. Wait for setup to finish before
continuing; native startup commands run after the entry breakpoint is installed.

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

Stacks are distributed by default. To inspect only frames in the selected
thread, pass `--stack-mode local` in managed or connect mode. The stack pane is
then labeled `DDB Local Call Stack`; `d` and the toolbar refresh that local stack.
Local mode uses the paginated `ListFrames` API, retaining native frame handles
for source, variables, registers and memory. It does not change execution scope
or all-stop behavior, and never activates automatically after a distributed
backtrace failure. Omit the option or use `--stack-mode distributed` for the
normal DDB-native view.

| Input | Action |
|---|---|
| `Tab` / `Shift+Tab` | Move panel focus |
| `j` / `k`, Up/Down, mouse wheel | Navigate the focused panel |
| `Ctrl+P` / `Ctrl+N` outside Source | Move up / down, including navigation overlays; does not edit prompts |
| Page Up / Page Down | Move by the focused pane's visible page size |
| `Enter` / click | Select a DDB group/session/thread target, inspect a thread/frame, expand a variable, or activate a row |
| `F5` | Continue all sessions by default |
| `F6` | Interrupt/pause all sessions by default |
| `F10` | Step over in the selected paused frame's thread |
| `F11` / `Shift+F11` | Step in / step out |
| `c` | Override controls: Auto → thread → session → group → all eligible sessions → Auto |
| `b` | Toggle a breakpoint at the source cursor |
| `B` | Create a conditional, temporary, or hardware breakpoint |
| `Space` in the target picker | Select one or more session/group targets, or server-resolved broadcast |
| `x` / `Space` | Enable or disable the selected breakpoint |
| `Delete` | Remove the selected breakpoint |
| `F` | Add a function/symbol breakpoint, then choose session/group targets |
| `d` | Refresh the selected stack mode, distributed by default |
| `Enter` in Variables | Load/collapse a scope or expand/collapse variable children |
| `e` | Evaluate an expression |
| `w` | Add a retained watch expression in Variables |
| `W` / `Edit [W]` in Variables | Edit the selected watch or a watch child's owning expression; Esc cancels |
| `Delete` in Variables | Remove the selected watch or dismiss the Inspect result |
| `m` | Read memory (`ADDRESS` or `ADDRESS ; BYTE_COUNT`) |
| `E` / source `Inspect [E]` | Inspect an expression once in the selected frame; expand its result in Variables |
| `v` in Topology / header `Flat [v]` or `Group [v]` | Toggle grouped/flat session view without changing execution scope |
| `Space` / `Enter` in Topology, or click a disclosure arrow | Expand/collapse the selected group/session without changing execution scope |
| `Left` / `Right` in Topology | Collapse or visit the parent / expand or visit the first child |
| Click a session label, then `s` | Choose a signal for exactly that session; disclosure clicks only fold |
| `i` in Topology / right-click a row | Open a frozen canonical group/session details snapshot; arrows or hjkl, PageUp/PageDown, Home/End and wheel scroll; Esc or right-click closes |
| `e` in topology details / `Export` button | Export the captured JSON snapshot to a new local file; enter a path and press Enter, or Esc/empty Enter to cancel |
| `y` in topology details / `Copy` button | Send the captured JSON to the terminal clipboard using OSC 52 |
| `K` in Topology / `Kill [K]` | Confirm SIGKILL for one selected session or a thread's owning session; `y`/Terminate accepts, Enter/Esc cancels |
| `v` in Breakpoints / footer `Files [v]` or `Flat [v]` | Toggle full-path file grouping; preserve the selected breakpoint/member and its target |
| `H` in Breakpoints / `Paused [H]` or `All [H]` | Switch all breakpoints / currently paused hits grouped by DDB group and session |
| `M` in Variables | Read 256 bytes at the selected variable/watch's DDB-provided address |
| `g` | Go to a source line (loads another source window when needed) |
| `h` / `l` or Left/Right in Source | Scroll long source lines horizontally, keeping the gutter fixed; `j` / `k` or Down/Up move the cursor |
| `Home` in Source | Return to the left edge of the source text |
| `o` | Open the source explorer at local and DDB-exported roots |
| `Ctrl+P` in Source | Search debugger-known files across DDB sessions |
| `f` in Source | Resume following the selected thread's execution location |
| `Alt+Left` / `Alt+Right` | Navigate source history without changing debugger selection |
| `J` | Open the execution-jump prompt |
| `s` | Send a signal to the selected thread |
| `a` / `Enter` in Extensions | Invoke an action declared by the active DDB extension |
| `Left` / `Right` in Extensions | Select an extension action without invoking it |
| `Shift+Left` / `Shift+Right`, Shift+horizontal wheel in Extensions | Scroll the selected action label; `Shift+Home` resets its horizontal position |
| `Up` / `Down`, Page Up/Down in Extensions | Scroll extension data, even when actions are available |
| `Home` / `End` in Extensions | Jump to the first/last page of extension data; `Shift+Home` still resets only the action label |
| `:` | Run a native GDB/LLDB command in the selected frame |
| `r` | Refresh all state |
| `Alt+1` … `Alt+9`, `Alt+0` | Focus the numbered pane; numbers remain stable when optional panes are hidden |
| `Ctrl+u` / `Ctrl+d` in Source | Move half a visible page, loading another source window when necessary |
| `?` | Open the categorized help overlay; `/` searches shortcuts and descriptions |
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
Use `--watch-file PATH` to retain expressions across TUI restarts. Without this
option, watches remain process-local. Expressions use debugger syntax and may
have side effects; prefer expressions that only read state.
Editing preserves the watch's identity and only evaluates that changed expression.
Other watches keep their values and expanded children. Old replies cannot replace
the edited result. Edits made while running wait for a stopped frame.

For example, reuse this file when starting the same project:

```sh
ddb-tui --watch-file .ddb/watches.json .ddb/dbg_grpc.yaml
```

The parent directory must already exist. The file may be new; adding, editing,
or removing a watch saves it automatically in the background. Normal shutdown
flushes the latest change. Only expressions are saved, never values, handles,
session IDs, addresses, or API credentials. Loaded expressions are evaluated
when an inspectable stopped frame is selected. Only pass files you trust.

The versioned format is:

```json
{"version":1,"expressions":["counter","request"]}
```

Files are limited to 1 MiB, 64 expressions, and 4096 bytes per single-line
expression. Invalid files stop startup before a managed backend is launched.
Saves use atomic replacement and owner-only permissions on Unix. A persistent
`.lock` sidecar uses an OS advisory lock, released when the process exits, even
after a crash. Do not delete it while a TUI is using the file. A second TUI cannot
open the same watch file; use a separate file for an independent watch list.
Detected external edits are not overwritten. Save errors appear in the TUI and
are reported on shutdown if the latest change remains unsaved. The lock does
not prevent unrelated editors from writing, so edit the file while the TUI is
closed. Expressions can contain sensitive data; keep private watch files and
their lock sidecars out of version control.

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

Source breakpoint creation asks DDB's `ResolveSourceTargets` API for the sessions
and groups that actually use the exact debugger source path. The picker does not
infer ownership from local filenames or show unrelated sessions. Loading,
incomplete discovery, and errors appear inside the picker. Function breakpoints
retain explicit symbol-based session, group, and broadcast scopes.

Source horizontal scrolling also accepts horizontal mouse-wheel events. It
preserves the source cursor and execution-following mode. Navigation history
restores each file's horizontal offset; resizing clamps it to the content width.
Vertical movement keeps the viewport stable until the cursor reaches an edge.
Loading another source window preserves that edge instead of recentering the
cursor; pending loads use the current pane height if the terminal is resized.

`?` opens categorized, scrollable help. Press `/` to search categories, shortcut
keys, or descriptions. Search words match regardless of case or order. Enter
keeps the filter, Backspace edits it, and Ctrl+u clears it while editing.
Esc first stops search editing; when not editing, Esc or ? closes help.
Use Up/Down, Ctrl+P/Ctrl+N, j/k, Page Up/Down, Home/End, or the mouse wheel to scroll. Help wraps
to the terminal width and blocks clicks on debugger controls underneath it.

Direct pane shortcuts are Alt+1 Topology, Alt+2 Breakpoints, Alt+3 Source,
Alt+4 Stack, Alt+5 Variables, Alt+6 Registers, Alt+7 Memory, Alt+8 Timeline,
Alt+9 Output, and Alt+0 Extensions. Hidden Timeline and Output panes open when
selected; unavailable inspection panes report why they cannot open. Shortcuts
do not switch panes while editing a prompt or interacting with a modal dialog.
No prefix sequence is required. In tmux or a terminal emulator, reserve these
Alt-number combinations for DDB rather than binding them to another action.

Toolbar buttons have explicit boundaries and action names. Press `?` for shortcuts.
The first group contains execution actions: Continue, Pause, Step over, Step in,
and Step out. Frame source, DDB stack, Refresh, and control scope form the next
group. Both groups share a row when they fit; whole buttons wrap on narrower
terminals. The status row shows connection state and running/stopped totals,
without repeating the Auto scope explanation. Gaps and the status row are not
clickable. Unsupported controls remain hidden.
Below 19 terminal rows, the existing compact header and keyboard controls apply.

The toolbar starts in `Scope: Auto`. Continue and Pause broadcast to all sessions;
Step over, Step in, Step out, and other stepping actions use the selected paused frame's
owning thread, including a frame in another session's distributed stack. Without
a frame, stepping uses the selected paused thread. A distributed boundary is not
an executable frame. Browsing a group or session does not change these defaults.
Press `c` to request an explicit control scope, then cycle back to `Auto` to
restore the defaults. Signals keep their explicit single-session selection.

The toolbar shows actions supported by DDB for each resolved target. A thread-only
Next action is not offered while a session or group override is active. Stack
frames without a usable source file/line are dimmed and labeled `no source
location`. A reported location starts as `source unchecked`; the active read is
`loading source`. Failed loads dim the function name and show `source load
failed`, with the reason at the end of the horizontally scrollable row. The
Source pane also shows the error. A failed load is not a permanent claim about
the file: select the frame and press Enter or F8 to retry. A successful read
removes the failure/unchecked label.

Read evidence is scoped to the DDB instance, owning session, and exact debugger
path. The same path in another session is independent. Stale/cancelled replies
cannot change the indicator; session retirement and backend replacement discard
the corresponding evidence. The cache is bounded to 128 owner/path entries.
Other frames are not eagerly fetched, so this indication adds no source probes
to pause or distributed-backtrace refreshes.

If an explicitly overridden execution target disappears, controls stay disabled
and the scope is labeled unavailable until you select a target or cycle scope.
Inspection may move to a surviving thread, but it cannot silently redirect
overridden controls or signals. Automatic Continue/Pause still resolve all
current sessions. A retired target does not reactivate merely because the
same ID reappears or a replacement backend uses an identical ID. Scope cycling
from a thread follows the control thread's owner, not a different inspected
distributed frame.

The bottom line shows the latest user mutation as `queued`, `pending admission`,
`pending`, `completed`, `error`, or `cancelled`. It captures the requested target
when submitted using its developer-facing name. Continue,
stepping, jumps, signals, breakpoint changes, evaluations, assignments, native
commands, extension actions, and logpoint configuration use this feedback.
An animated spinner advances every 100 ms while a tracked action or source,
frame, or variable read is pending. Animation is local and sends no API requests.
Pending mutations take priority over read activity. Empty results, read errors,
queue rejection, and disconnection end the corresponding read activity.
Snapshots do not replace mutation outcomes. The latest outcome stays visible
until another action is submitted, except while a background read needs feedback.
Timeline records each transition; concurrent pending actions remain counted even
when the latest action has completed. Normal controls, feedback, and hit lists
omit operation IDs and stop revisions. `--trace-api` exposes those diagnostics,
plus endpoint/protocol details, when troubleshooting.

An admitted operation whose wait times out is `outcome unconfirmed`, not an
execution failure. Connection loss also leaves pending outcomes unconfirmed.
There is no automatic retry. A final receipt from the same backend can resolve
the outcome; a replacement backend cannot complete the old backend's intent.
Partial fanout failures are reported as errors with failed-target counts.
History is bounded to 64 intents; active intents are never silently evicted.

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

`E` or the source header's `Inspect [E]` button opens a one-shot expression prompt.
It captures the selected stopped frame, including a distributed caller, and uses
DDB's `HOVER` evaluation context. Its `Inspect:` result supports the same child
expansion and memory action as variables. Delete the result row to dismiss it.
Changing frames or resuming clears it; it is not automatically re-evaluated like
a watch. No expression is evaluated just because the mouse moves over source.

`M` in Variables reads 256 bytes at the selected variable or watch's API-provided
address, using the owning distributed frame. Values without an address report
that limitation; the TUI does not interpret their display text as an address.
GDB and LLDB supply native storage addresses for addressable values. GDB resolves
only native symbol/field/pointer/array paths and does not re-evaluate a watch to
find its address. Register-only, unavailable, synthetic, and unsupported expression
values can have no address; use `m` when you want to enter an address explicitly.

`m` reads memory and focuses its pane. Memory has independent row selection;
Up/Down, Page Up/Down, and the mouse wheel reach every returned row. Left/Right
reveals the hex and ASCII columns without wrapping them. Use `z` or the pane's
Expand button for a full-width view. New reads reset the view; changing frames
or resuming execution clears stale memory. Distributed-frame reads target that
frame's owning thread, not the thread where the distributed stack began.

Topology details export uses canonical v2 resources, not legacy numeric IDs or group
hashes. The exported document is the snapshot you opened, even if sessions change
while entering its destination. Relative paths resolve from the TUI's working
directory; use an absolute path to choose another location. Parent directories must
exist. Paths are literal: `~` and environment variables are not expanded. Existing
files, directories and symlinks are never overwritten. New files are owner-only on
Unix. Wait for the `Details exported to …` message before quitting. Export is local
to the frontend, does not require a live backend, and does not write the clipboard.

For direct copying, press `y` or click Copy in the details dialog. This sends the
same captured JSON to the terminal host's clipboard, without contacting DDB or
changing the selected target. Nothing copies on selection, hover or rendering,
and the TUI never reads clipboard contents. Copy is limited to 32 KiB of UTF-8
data; larger snapshots must be exported instead of silently truncated.

Copy uses Crossterm's [OSC 52 clipboard command](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands).
Your terminal and any multiplexer must support and permit it. The status reports
that the request was sent, not that the clipboard accepted it, because the write
has no acknowledgement. If paste does not work, use Export. Write errors are
reported and are not retried automatically. This targets the clipboard of the
terminal displaying the TUI, including when the TUI runs over SSH.

In Breakpoints, Stack, Variables, Registers, and Memory, Left/Right or a horizontal
mouse wheel reveals clipped names and values. Topology uses Shift+Left/Right for
horizontal scrolling because plain Left/Right navigates the tree. Home returns to the start. Each pane remembers its position;
resizing clamps scrolling to the available content. Horizontal navigation does
not select another frame or issue debugger commands.

Topology groups and sessions have disclosure triangles. Clicking a triangle or
pressing Space/Enter folds only that node. Labels select the explicit group,
session, or thread for execution controls. Folding preserves the control target
and inspected thread, even if their row becomes hidden. The highlight moves to
the nearest visible ancestor; the toolbar continues to name the actual control
scope. Right expands a folded node or visits its first child. Left collapses an
expanded node or visits its parent. Fold preferences survive grouped/flat view
changes and refreshes, are pruned when resources disappear, and reset for a new
DDB server instance. Revealing a paused breakpoint hit opens its hidden ancestors.

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
Press `F8` from any pane, or click `Frame source` in the toolbar, to open
or retry the selected stack frame's source file at that frame's line. `F8` is a
keyboard shortcut, not a frame number. It navigates only: it does not continue,
step, or select a different stack frame. Unlike
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

The source title identifies the document's owners. Markers require the exact
declared debugger path; equal filenames or path suffixes do not establish a
relationship. Remote documents and local files with session tags or frame-owner
hints also require a matching owner. An untagged local root opened independently
of a frame shows breakpoints and execution locations for that exact path across
sessions. This aggregate view does not infer ownership or select a breakpoint
target. A mapped workspace file uses `debugger_path` for breakpoints, never its
display path. Without that mapping, source breakpoint placement is rejected.

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
or session preserves the inspected thread. Selecting a thread inspects it.
Topology selection changes control targets only while an explicit override is
active; `Auto` keeps Continue/Pause global and stepping frame-owned. Controls
require the action/scope combination advertised by `GetCapabilities`.

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

Breakpoint rows keep their first-seen order while frames, state snapshots, or
hit counts change. New breakpoints and installed members append; simultaneous
new rows use identity order. File grouping still sorts full paths and lines.
Selection follows breakpoint/member identity, not the incoming array position.

Rows start with `●` for enabled or `○` for disabled, followed by their source
location or symbol. A right-aligned `[1 hit]` or `[N hits]` badge counts threads
currently stopped there. Installed member rows show that badge for a matching
live hit; otherwise they show their session's current state, such as `[running]`,
`[stepped]`, `[paused]`, `[signal]`, or `[exception]`. `[stepped]` means a step
completed and execution is stopped. `[mixed]` means threads have different
execution states or principal stop reasons. `[unknown]`, `[unavailable]`, and
`[offline]` do not invent a known stop cause. A breakpoint cause that cannot be
attributed to this particular member is `[bp stop]`. Inspecting another frame
never changes these backend-derived states.
Badges stay visible while row text scrolls horizontally when the pane is wide
enough. Clipped labels have an ellipsis. Expand with `z` or scroll horizontally
to read long labels and diagnostics. The muted `total hits:N` values come from
DDB: the logical row counts observed breakpoint stops across its lifetime,
and each member counts its own observed stops. Retiring a member does not lower
the logical total. Native encounters skipped by an ignore count or condition
are not DDB-observed stops. Stepping, refreshes, and all-stop peers do not add
hits. These counters are history, not current status.
Member rows show current hits only when
DDB identifies that member or there is a single member in the hitting session.
Live hit badges disappear when the thread resumes or the backend disconnects.
The first observed ordinary stop selects its owning thread in Topology, expands
collapsed ancestors, and follows its frame and source automatically. Further
concurrent stops do not steal that selection while the first owner remains
paused. The Breakpoints title and rows count current hits; `H` and `P` let you
choose another. This is first-observed ordering, not a comparison of wall clocks
across machines. A bootstrap or recovery snapshot containing several existing
stops uses their snapshot order. Resuming or a successfully queued user execution
command starts a new focus decision. A rejected command does not discard the
current owner.
Press `P` in Breakpoints, or right-click a breakpoint row, to choose a currently
paused hitting thread. Enter or a click loads that thread's stack, source and
variables. Esc or Cancel closes the picker. The picker validates the stop revision
before navigating and rejects hits that resumed or changed while it was open.
This navigation does not change an explicit control override. In `Auto`,
stepping uses the newly inspected frame; Continue/Pause still broadcast.

For a persistent view of all current breakpoint stops, focus Breakpoints with
Alt+2 and press `H`, or click `Paused [H]`. It shows hitting threads grouped by DDB
group and session, excluding historical hits and automatically paused all-stop
peers. Up/Down, PageUp/PageDown, and Home/End choose hits; Enter or a click opens
the owning frame through ordinary DDB inspection. The mode stays active when
switching panes and updates as threads stop/resume. A resumed or replaced stop
loses its selection rather than silently selecting another thread. Disconnection
removes all unverified hits; a new backend discards the old selection.

The live hits view is navigation-only. Breakpoint mutation keys do not act on the
hidden all-breakpoints selection. Press `H` or `All [H]` to manage breakpoints;
the previous logical breakpoint/member selection and grouping remain intact.
Browsing hits does not change an explicit execution-control override.

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
- Top: capability-gated controls, Auto or an explicit override, and connection state
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
CARGO_PROFILE_DEV_DEBUG=2 CARGO_PROFILE_DEV_OPT_LEVEL=0 \
  cargo build --manifest-path ../ddb/core/tests/fixtures/real_dbt/Cargo.toml \
  --target-dir ../ddb/core/tests/fixtures/real_dbt/target
cargo test --test e2e_mock -- --ignored --skip greeter_auto_attach --test-threads=1
```

The PTY suite currently targets Unix and requires `script`, GDB, and LLDB.
`DDB_E2E_BIN` may be set to test a different DDB executable.

The real distributed-memory cases use two isolated fixture processes. Setup uses
the v2 SDK to capture the caller context, resume it, and pause it at another PC.
The TUI then selects the callee thread, reconstructs its distributed stack,
opens a non-top caller frame, expands a variable, and reads its storage through
`M`. Both GDB and LLDB must show the caller's expected bytes. Literal watches and
failed evaluations must reject `M` without switching to Memory. Each workflow
also checks a second TUI in explicit local mode: the same callee
retains source and variable inspection while the cross-session caller is absent.
Run only these cases with `cargo test --test e2e_mock distributed_caller_memory -- --ignored --test-threads=1`.

The opt-in auto-attach greeter regressions additionally require an instrumented
gRPC helloworld checkout, `ddb_runapp`, and Mosquitto. They reserve a free loopback
broker port, preserve and restore the prior `/tmp/ddb/service_discovery/config`
bytes, and refuse an occupied configured broker port. They own their brokers
and applications; do not run another discovery session while these tests run.

```bash
DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
  cargo test --test e2e_mock greeter_auto_attach_all_stop_terminal_workflow \
  -- --ignored --test-threads=1
```

Use the `greeter_auto_attach` filter to also run the server-only and two-session
client source-readiness regressions. Add `--nocapture` to print stage timings.
Source readiness measures navigation through the rendered cursor; all-stop
measures continue through a canonical snapshot showing the server breakpoint
and every thread stopped. The subsequent hit-owner timing includes snapshot
verification and explicit owner navigation, not just terminal painting. These
tests retain the ordinary 15-second readiness deadlines.

`greeter_auto_attach_single_config_managed_terminal_workflow` launches through
`ddb tui` with one project file and no source-workspace or session-profile flags.
It browses local sources before attach, automatically attaches both greeters,
sets and renders a breakpoint from the untagged local source tree, and continues
all sessions without a scope workaround. The RPC hit must select its server
thread and source automatically before any manual hit navigation. The test then
inspects an exact `"Hello "` string, steps over the assignment using the frame's
thread, refreshes without losing formatting, and checks owned-broker shutdown.
It requires trusted GCC printers at `/usr/share/gcc/python`.
Set `DDB_GREETER_PROJECT_CONFIG=/absolute/path/to/helloworld/.ddb/ddb.yaml`
to exercise an existing project unchanged through direct `ddb-tui`, including
its inline `ddb_path` and config-relative source roots. The project must use
the free configured loopback Mosquitto port and the `helloworld` local root.

The `greeter_auto_attach_pretty_string_terminal_workflow` case uses the explicit
libstdc++ profile above and requires its formatter package on the DDB host. Set
`DDB_GREETER_FORMATTER_SETTINGS=/absolute/path/to/settings.yaml` to test a
different trusted installation profile. It asserts an exact `"Hello "` watch
summary, steps from the message assignment to the return, and verifies the
summary again. A raw structure merely containing that text does not pass.
