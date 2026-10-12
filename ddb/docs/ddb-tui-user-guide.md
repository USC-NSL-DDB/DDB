# DDB and ddb-tui user guide

This guide covers the paired two-binary debugger workflow. DDB owns debugger
sessions and the public API; `ddb-tui` owns terminal interaction and, in managed
mode, supervises one private DDB child. They never use an in-process debugger
shortcut.

## Install and verify

Extract a paired archive and put its `bin` directory on `PATH`, or invoke the
binaries in place. Keep `ddb` and `ddb-tui` from the same archive together so
sibling discovery and API compatibility are deterministic.

```bash
./bin/ddb --version
./bin/ddb tui --help
./bin/ddb-tui --help
./bin/ddb tui ./examples/managed/mock.yaml
```

Real sessions require GDB or LLDB and a program built with debug information.
Use an unoptimized/development build while validating source stepping.

## Choose an ownership mode

### Managed configuration: the default

```bash
ddb tui ./debug.yaml
# Equivalent direct form:
ddb-tui ./debug.yaml
```

The TUI starts `ddb serve`, uses authenticated loopback port `0`, waits for its
atomic startup report, verifies API v2 and schema compatibility, and shuts down
only that owned backend. DDB alone parses the backend configuration. Local, remote-
target, and distributed configurations all use this one-command form.

`--config ./debug.yaml` is equivalent to the positional path. Use `--ddb-path`
only for source-tree development or deliberate binary selection. Use
`--startup-timeout SECONDS` for unusually slow hosts and `--backend-log PATH`
to retain backend output at a specific new path.

### One file for the project

Put persistent GDB/LLDB setup under `Conf.Debugger` and frontend preferences
under `Tui` in that same YAML. The frontend reads its `Tui` section using the
shared `ddb-config` schema. DDB owns native session startup and does not open
frontend-local source paths. For example:

```yaml
Conf:
  Debugger:
    backend: gdb
    gdb:
      commands: [set print elements 33]
Tui:
  source_roots:
    - name: application
      path: ../src
      debugger_path: /build/application
  stack_mode: distributed
  source_search: exact
```

`o` browses this local tree before any target attaches. `Tui.source_roots` does
not export files through the backend; DDB `SourceRoots` remains an explicit
backend-host export. Relative paths in these sections resolve against the
containing YAML. Source mappings in `Conf.Debugger` refer to absolute
debugger-host paths instead.

Only the selected backend's commands run, once per session bootstrap, including
automatic attachment. Refreshing or reconnecting the TUI does not repeat them.
Only use trusted configurations; native commands can control the debuggee.
Unknown fields and invalid values are rejected.

Optional `--tui-config preferences.yaml` reads frontend-only fields without a
`Tui` wrapper. Defaults are overridden by project `Tui`, then the preferences
file, then explicit CLI options. `--ddb-set Conf.Debugger.backend=lldb` forwards
a backend override to DDB's canonical parser. Managed transport/authentication
and frontend settings cannot be changed with that option. Separate
`--source-workspace` files and API-based `--session-profile` are optional, not
required for the normal project workflow.

The [complete configuration reference](../../ddb-tui/README.md#one-project-configuration)
and [instrumented helloworld example](../../ddb-tui/examples/helloworld.yaml)
include C++ printer setup and automatic discovery.

### Launch a local program

```bash
ddb tui launch --backend gdb -- ./target/debug/app --flag "value with spaces"
```

The `--` separator is required. Arguments after it remain an exact vector and
are not interpreted by a shell. Launch stops at entry and kills the launched
program on TUI exit by default. `--no-stop-at-entry` starts immediately.

### Attach to an existing process

```bash
ddb tui attach --backend lldb --pid 12345
```

Attach detaches on TUI exit and leaves the original program alive. On Linux,
ptrace/Yama, container capability, or ownership policy may reject attachment.
DDB reports the policy failure and never attempts privilege escalation or
changes host security settings.

### Connect to an externally owned backend

```bash
ddb-tui connect --api https://debug.example.com --token "$DDB_API_TOKEN"
```

Use connect only when a service manager, container, Kubernetes workload, shared
team service, or automation owns DDB. Quitting does not stop that backend. The
`ddb-tui --api URL` shorthand remains supported. Both managed and external
connections require the current API v2. There is no v1 fallback; upgrade paired
backend/frontend binaries together when the contract changes.

### Headless backend

```bash
ddb serve ./debug.yaml
```

This API-only mode stays alive when stdin closes. Use it for community clients,
automation, and externally managed deployments. Existing `ddb ./debug.yaml`
keeps the legacy stdin command loop and API behavior.

## Distributed sessions and lifecycle policy

API service readiness is intentionally separate from target readiness. The TUI
opens as soon as DDB can answer authenticated API requests; healthy targets
appear while slower or failed targets continue to report connection state and
events. A delayed remote target does not block the entire frontend.

### DDB-native interaction model

The toolbar groups bounded, labeled execution and navigation buttons, sharing
a row when they fit. Whole buttons wrap on narrower terminals. Press `?` for
their shortcuts. `Frame source` or F8 opens or retries the selected stack frame's
source at its line. F8 is a shortcut, not a frame number, and the action does not
resume or step execution. `f` instead follows the selected thread's execution location.

The left topology is DDB group → session → thread. Controls start in `Auto`.
Continue and Pause broadcast to all sessions. Stepping uses the selected paused
frame's owning thread, or the selected paused thread if no frame is loaded.
This also applies to a caller frame in another session's distributed stack.
Boundary rows cannot be stepped. Browsing a group or session does not change
these defaults. `c` explicitly overrides controls through thread, session, group,
and all eligible sessions, then returns to `Auto`. While an override is active,
topology selection changes that target. Unsupported action/scope combinations
are hidden and rejected before queueing, according to `GetCapabilities`.

The first observed ordinary stop selects its actual owning thread, expands its
topology ancestors, and loads its frame and source. Other concurrent stops do not
steal focus while that owner remains paused. Breakpoint rows and the panel title
count current paused hits separately from historical hit counts. Use `H` for the
live hit view through `Paused [H]`, or `P` to choose a hit. Enabled/disabled
breakpoint dots remain at the start of each row. Right-aligned `[1 hit]` or
`[N hits]` badges show current matching hits. Member rows otherwise show their
session's backend-derived state, including running, stepped, paused, signal,
exception, or mixed. A completed step is stopped, not "stepping". Unknown or
unavailable causes remain explicit. Muted `total hits:N` values show DDB-observed
stop counts for the logical breakpoint and each installed member. Ignored native
encounters, steps, refreshes, and all-stop peers do not increment them. Retiring
a member preserves the logical lifetime total. Horizontal scrolling preserves
the badge, and the grouping control stays in the footer rather than covering
the title. Automatically paused peers are not counted as breakpoint hits.
Existing stops received together at bootstrap or recovery
use snapshot order; DDB does not compare remote wall clocks to order them.

Breakpoint and member rows retain their first-seen order across frame inspection
and snapshot refreshes. New rows append without moving existing rows. File
grouping remains explicitly sorted by full path and line. Selection stays tied
to breakpoint/member identity.

Use Ctrl+P to move up and Ctrl+N to move down in every non-Source panel and
navigation overlay. Source uses hjkl and arrows; Ctrl+P there still opens the
debugger file catalog. These navigation aliases do not alter prompt editing.

The center stack is DDB distributed backtrace by default. Selecting a stopped
thread or receiving a stop refreshes its cross-session call chain automatically.
Boundary rows explain transitions but are not debugger frames; ordinary rows
retain their owner session/thread and can be selected for source, variables, and
registers. `TRUNCATED` remains in the title until the next inspection when DDB
returns a bounded result, with the reason also recorded in status and timeline.

`b` and `B` open a target picker for server-resolved broadcast, group-inheriting,
individual-session, or explicit multi-target breakpoints as supported. The
breakpoint manager shows the aggregate plus per-session verification, pending,
hit, condition, ignore-count, and diagnostic state. Optional framework features
come from public extension descriptors/state/actions instead of hard-coded TUI
knowledge. The complete keyboard, mouse, and gutter reference is in the
`ddb-tui` README.

Source browsing is independent of frame inspection. Use `o` or the source
panel's Open button for local roots and configured DDB `SourceRoots`, and `Ctrl-P` for the
debugger catalog. The source title identifies its owners; `f` returns to the
selected thread's execution location. A breakpoint uses the document's declared
debugger path, and its target picker defaults to the document's single owner,
not whichever thread is selected. Unmapped files cannot create source
breakpoints. Multiple matching logical breakpoints require a choice in the
breakpoint manager before deletion.

An untagged local root opened without a frame-owner hint shows exact-path source
markers across sessions. It does not imply a single owner; choose breakpoint
targets from DDB's discovered candidates. Tagged local roots and remote source
documents retain owner-specific matching.

Pending controls, mutations, and source/frame/variable reads show an animated
spinner. Its 100 ms animation is local and adds no API polling. Normal feedback
uses action names and developer-facing target labels. Use `--trace-api` for
operation IDs, stop revisions, and connection diagnostics.

Prefer inline `Tui.source_roots`. Optionally pass `--source-workspace ./workspace.yaml` to `ddb tui` or `ddb-tui` for explicit
directories on the frontend host. This separate file contains a `roots` list of
`name`, `path`, optional `debugger_path`, and optional `session_tags`. Paths are
relative to that file. DDB exports and local directories are labelled separately;
the backend never receives the local grants. Local documents and history survive
backend replacement. See the [source explorer configuration](../../ddb-tui/README.md#source-explorer)
for examples, mapping precedence, resource bounds, and keyboard/mouse controls.

The existing global policy remains the compatibility default:

```yaml
Conf:
  on_exit: detach
```

Use a per-session override for mixed launch/attach ownership:

```yaml
StaticSessions:
  - tag: launched-worker
    start_mode: binary
    binary_path: ./target/debug/worker
    on_exit: kill
  - tag: attached-service
    start_mode: attach
    pid: 12345
    on_exit: detach
```

Managed transport settings override configured API bind, port, and authentication
only. They do not reinterpret topology, discovery, debugger, or lifecycle fields.
Remote API exposure requires an explicitly deployed `ddb serve` with the
authentication, TLS, proxy, and CORS controls in the API deployment guide.

## Security model

Each managed run creates distinct control and admin tokens in a private runtime
directory. Tokens are passed by file path, loaded into DDB memory, and the token
document is unlinked before readiness. Values are redacted from argv, environment,
startup reports, logs, UI state, and diagnostics. The API listener is loopback-
only and OS-assigned; configuration cannot widen it in managed mode.

The frontend uses the control token for debugging. Its supervisor alone retains
the admin token needed for owned-backend shutdown. A startup or compatibility
error shows versions, phase/code, endpoint, exit status, and retained log path,
but never expression values, memory, source contents, or bearer values.

## Failure recovery

| Symptom | Meaning and recovery |
|---|---|
| `ddb-tui` not found | Install the paired archive, keep both binaries beside each other, or set `DDB_TUI_PATH`/`--ddb-path` only to a trusted executable. |
| `CONFIG_INVALID` | Correct the named YAML field/session. DDB is the schema authority; the TUI does not rewrite it. |
| `DEBUGGER_UNAVAILABLE` | Install the selected GDB/LLDB or choose the available backend; verify `gdb --version` or `lldb --version`. |
| `AUTH_SETUP_FAILED` | Check that the managed runtime parent is writable and supports private regular files. Do not replace token files with symlinks. |
| `API_BIND_FAILED` | In headless mode, choose a free bind/port. Managed mode already uses loopback port `0`, so inspect the retained log for host policy/resource failure. |
| Startup timeout | Increase `--startup-timeout` only after checking the stable phase and retained backend log. The timed-out process group is terminated and reaped. |
| Attach rejected | Confirm PID ownership and host/container ptrace policy. Apply policy changes yourself only if appropriate. |
| `reconnecting` | The API/event stream was interrupted. The v2 SDK replays from acknowledged cursors; keep healthy distributed targets available while the backend recovers. |
| Backend exited | The TUI remains terminal-safe, identifies the nonzero status, and retains the backend log. Restart the one-command workflow after addressing the backend cause. |
| Version/schema mismatch | Replace both binaries from one paired archive. Managed mode refuses unsafe fallback or an endpoint whose identity/version changed. |

`q`, keyboard Ctrl-C, SIGINT, SIGTERM, and terminal SIGHUP all restore the terminal
and clean up an owned backend. Parent-liveness handling prevents an abrupt TUI
exit from silently orphaning DDB. Generated attach is the expected exception:
the original debuggee remains alive and is explicitly detached.

If a third-party terminal or host crash left display modes visibly wrong, run the
terminal's normal reset command after confirming no debugger process remains.
The supervisor's `--backend-log` path is never overwritten; choose a new path for
each retained run.

## Upgrade, compatibility, and rollback

1. End or deliberately detach active sessions; do not replace binaries underneath
   a running managed debugger.
2. Inspect the new archive's `manifest.json` for both versions, target triple,
   API versions, schema range, binary list, license list, and release eligibility.
3. Extract to a new versioned directory and run the packaged Mock example before
   changing the active symlink or `PATH` entry.
4. Switch `ddb` and `ddb-tui` together. Do not independently upgrade only one
   binary even when their current API ranges overlap.
5. Keep the previous extracted directory until real launch/attach smoke checks
   pass in your environment.

For rollback, stop the current pair and atomically point `PATH`/the installation
symlink back to the previous paired directory. Roll both binaries back together.
DDB configurations remain backend-owned text and should be version-controlled;
revert config/schema changes with the same release if the older DDB does not
accept them. An external connect deployment may temporarily mix versions only
when the reported API/schema ranges overlap; diagnostics are authoritative and
there is no silent managed downgrade.

## Uninstall

Stop managed sessions first, then remove the paired installation directory or
both installed binaries, the optional Bash completion, and any operator-created
backend logs. Managed runtime credentials are temporary and are removed on normal,
signal, panic, and terminal-loss paths. Do not delete externally owned DDB state,
service-manager units, shared credentials, or debuggee processes unless those
resources are explicitly part of the uninstall scope.

## Development and verification

From the repository root:

```bash
cargo build -p ddb --manifest-path ddb/Cargo.toml
cargo build --manifest-path ddb-tui/Cargo.toml
cargo build --profile dev \
  --manifest-path ddb/core/tests/fixtures/real_loop/Cargo.toml

DDB_TUI_PATH=ddb-tui/target/debug/ddb-tui \
  ddb/target/debug/ddb tui ddb/examples/managed/mock.yaml
```

The full native feature/contract matrix and current test evidence are in the
[DDB-native readiness report](ddb-native-tui-readiness-2026-08-16.md). Lifecycle
and packaging evidence remains in the
[integrated usability report](ddb-tui-integrated-usability-readiness-2026-08-15.md).
UI controls, gutter semantics, and PTY prerequisites are in the `ddb-tui` README.
