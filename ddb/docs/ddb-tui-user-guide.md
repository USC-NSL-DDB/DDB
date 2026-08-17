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
only that owned backend. DDB is the only configuration parser. Local, remote-
target, and distributed configurations all use this one-command form.

`--config ./debug.yaml` is equivalent to the positional path. Use `--ddb-path`
only for source-tree development or deliberate binary selection. Use
`--startup-timeout SECONDS` for unusually slow hosts and `--backend-log PATH`
to retain backend output at a specific new path.

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
legacy `ddb-tui --api URL` form remains supported. Explicit v1 fallback is for a
controlled migration of an external deployment only:

```bash
ddb-tui connect --api URL --api-version v1-fallback
```

Managed startup never falls back to v1. Authentication, permission, network,
malformed-protocol, or version errors also never trigger a downgrade.

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

The left topology is DDB group → session → thread, not a flat local-thread list.
Selecting a group or session changes the execution target while retaining the
thread under inspection; selecting a thread changes both. `c` cycles the target
through thread, session, group, and all eligible sessions. Unsupported actions
are hidden; an action on an unsupported scope is rejected before it can be
queued, according to `GetCapabilities`.

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
