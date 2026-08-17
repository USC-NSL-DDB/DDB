# DDB and ddb-tui integrated usability: implementation plan

Status: implemented release candidate; verified 2026-08-15. See
[the readiness and traceability report](ddb-tui-integrated-usability-readiness-2026-08-15.md).
Audience: DDB maintainers and coding agents implementing the user-facing debugger workflow
Scope: DDB CLI, `ddb-tui` CLI, managed backend startup, configuration-driven
debugging, process lifecycle, security, packaging, diagnostics, compatibility,
and end-to-end verification

This plan extends the
[community API platform plan](community-api-platform-implementation-plan.md).
That plan remains authoritative for API semantics, transport contracts, SDKs,
extensions, and community frontend compatibility. This document is authoritative
for how the two first-party binaries are launched and experienced together.

## 1. Outcome

DDB remains an independently runnable debugger backend and `ddb-tui` remains an
independently runnable frontend. They continue to communicate exclusively through
the public DDB API. The default user experience, however, becomes one command.

The primary configuration-driven workflow is:

```bash
ddb tui ./debug.yaml
```

The equivalent direct frontend invocation is:

```bash
ddb-tui ./debug.yaml
```

Both commands start a managed DDB backend, wait for it safely, connect the TUI,
and clean up according to explicit lifecycle policy. The same configuration can
still be used to run DDB without a frontend:

```bash
ddb ./debug.yaml
```

A distributed debug configuration is not a reason to require manual backend
startup. It follows the same one-command workflow. An explicit connection command
exists only when a DDB instance is deliberately owned by another lifecycle, such
as a service manager, Kubernetes deployment, shared team session, or long-lived
headless automation:

```bash
ddb tui connect --api https://debug.example.com
```

## 2. Decisions

1. **Keep two binaries.** `ddb` owns debugger sessions, domain state, API service,
   and headless operation. `ddb-tui` owns terminal interaction and managed-child
   supervision. They are packaged together but remain independently executable.
2. **Keep the public API boundary absolute.** Managed mode does not add an
   in-process shortcut, private socket protocol, shared mutable state, or private
   TUI-only debugger method. The official TUI continues to prove the community API.
3. **Make `ddb tui` a thin dispatcher.** It locates and executes `ddb-tui`, passing
   the path of the current `ddb` executable. It does not embed the TUI or implement
   a second supervisor.
4. **Make `ddb-tui` the supervisor in managed mode.** It starts `ddb serve`, owns
   the child handle, performs readiness and version checks, and applies shutdown
   policy. Direct `ddb-tui` and `ddb tui` therefore have identical behavior.
5. **Let `ddb-tui` accept DDB configuration files without interpreting them.** It
   canonicalizes and forwards the path. DDB alone parses, validates, defaults, and
   executes the backend configuration.
6. **Add a headless `ddb serve` mode.** A managed backend must not depend on an
   open stdin pipe or run the interactive stdin REPL. Existing `ddb <config>`
   behavior remains available and compatible.
7. **Use a machine-readable startup report.** Managed startup uses an ephemeral
   loopback port and an atomic startup-report file. It never scrapes human logs,
   assumes port 5000, or races to reserve a port in the frontend.
8. **Authenticate managed mode.** The launcher creates short-lived token material
   in a private runtime directory. It does not enable
   `api_insecure_allow_unauthenticated_v2` for real debuggees.
9. **Use stable HTTP/API v2 initially.** Managed startup is transport-neutral at
   the SDK layer, but this work does not depend on the optional gRPC preview.
10. **Treat launch topology and service ownership as separate axes.** Local,
    remote-target, and distributed sessions can all use a locally managed DDB.
    `connect` means “the backend lifecycle is external,” not “the debuggee is
    distributed.”

## 3. Requirements

Implementation PRs must cite the applicable requirement identifiers.

### UX-01: one-command default

- A user can start a configured local or distributed session with
  `ddb tui <config>` or `ddb-tui <config>`.
- The user does not select an API port, create an authentication file, start a
  second terminal, or wait manually for readiness.
- `launch` and `attach` shortcuts require no handwritten YAML.

### UX-02: independent first-class binaries

- `ddb` runs headlessly without `ddb-tui`.
- `ddb-tui connect` runs against any compatible externally owned DDB without
  requiring a locally installed backend.
- A missing companion binary produces an actionable error; neither executable
  downloads or executes unverified code automatically.

### UX-03: public API integrity

- `ddb-tui` imports only public API/SDK crates, not `ddb-core` modules.
- Every debugger read, mutation, event, and output item crosses the same public
  API available to a community frontend.
- Managed lifecycle messages are process-supervision concerns and may not carry
  debugger domain operations outside the API.

### UX-04: configuration parity and ownership

- A configuration accepted by `ddb <config>` is accepted by
  `ddb tui <config>`, subject only to safe managed-transport overrides.
- DDB is the sole parser and validator of the DDB configuration schema.
- `ddb-tui` passes the canonical file path and preserves the invoking working
  directory so current relative-path behavior does not silently change.
- TUI preferences are stored separately from DDB debugger configuration.

### UX-05: safe managed transport

- Managed DDB binds only to loopback initially and asks the OS for an available
  port by binding port `0`.
- An explicit runtime override takes precedence over configuration for the
  managed API bind, port, authentication file, and startup report.
- A configuration requesting remote API exposure is not honored in managed
  mode; the user is informed that external deployment requires headless DDB.
- Tokens never appear in command arguments, environment variables, logs, error
  messages, process titles, or diagnostic bundles.

### UX-06: deterministic startup and compatibility

- DDB atomically reports either ready metadata or a typed startup failure.
- Service readiness means the API can answer requests; it does not wait for all
  configured debug targets to attach or stop.
- The TUI verifies API major version and required capabilities before entering
  normal operation.
- A startup timeout terminates the managed child and restores the terminal.

### UX-07: explicit lifecycle

- Quitting the TUI gracefully shuts down only the backend it owns.
- `connect` never shuts down an externally owned backend unless the user invokes
  an explicit administrative action.
- Generated launch mode defaults to killing the launched debuggee on exit.
- Generated attach mode defaults to detaching and leaving the debuggee alive.
- Configuration-driven mode respects the configuration's declared exit policy.
- Signals, startup failures, TUI panics, and backend crashes cannot leave the
  terminal corrupted or silently orphan a debugger process.

### UX-08: distributed workflow

- One managed DDB can initialize every session and discovery provider in a
  distributed configuration.
- Target connection progress and failures appear as normal API state/events in
  the TUI; frontend startup does not block until every target is ready.
- No documentation implies that distributed debugging requires users to start
  DDB and `ddb-tui` separately.

### UX-09: CLI compatibility

- Existing `ddb <config> [existing options]` invocations retain their behavior.
- Existing `ddb-tui --api <url>` remains a supported alias for explicit connect
  mode for at least the current compatibility window.
- Existing explicit v1 fallback remains opt-in and is never selected merely
  because managed startup failed.
- Exit codes from the dispatched TUI are preserved by `ddb tui`.

### UX-10: packaging and version diagnostics

- Official release artifacts contain compatible `ddb` and `ddb-tui` binaries.
- Each binary can resolve its sibling when installed beside it without relying
  on `PATH`.
- Version mismatch errors show frontend version, backend version, supported API
  range, discovered API range, and remediation without exposing credentials.

### UX-11: observable and supportable failures

- Startup errors identify the failing phase: executable resolution, config
  validation, bind/auth setup, backend startup, readiness, API negotiation, or
  initial state hydration.
- Backend stderr is captured into a bounded buffer and retained log; it is not
  allowed to grow without bound in TUI memory.
- Diagnostics record paths, versions, exit status, timings, and stable error
  codes, but never token contents, raw expressions, memory, or source content.

## 4. User-facing command contract

### 4.1 Canonical commands

| Purpose | Unified command | Direct-binary equivalent |
|---|---|---|
| Managed configured session | `ddb tui debug.yaml` | `ddb-tui debug.yaml` |
| Same with explicit flag | `ddb tui --config debug.yaml` | `ddb-tui --config debug.yaml` |
| Launch an executable | `ddb tui launch -- ./app arg` | `ddb-tui launch -- ./app arg` |
| Attach to a local PID | `ddb tui attach --pid 1234` | `ddb-tui attach --pid 1234` |
| Connect to owned service | `ddb tui connect --api URL` | `ddb-tui connect --api URL` |
| Existing connect syntax | n/a | `ddb-tui --api URL` |
| Interactive/headless-compatible DDB | `ddb debug.yaml` | n/a |
| API-only DDB | `ddb serve debug.yaml` | n/a |

The positional config and `--config` forms are mutually exclusive. CLI parsing
must reject ambiguous combinations before starting any process.

### 4.2 Useful managed options

```text
--ddb-path PATH          explicit backend executable
--startup-timeout TIME   maximum wait for DDB service readiness
--backend-log PATH       copy or redirect the managed backend log
--api-version POLICY     v2 or explicit v1-fallback
--keep-backend           opt into a persistent managed session (later stage)
```

Debugger selection and startup options are available to shortcut modes:

```bash
ddb tui launch --backend gdb --stop-at-entry -- ./target/debug/app arg
ddb tui attach --backend lldb --pid 1234
```

The `--` separator is mandatory before the debuggee command so debuggee arguments
cannot be confused with TUI, DDB, or debugger options.

### 4.3 Executable resolution

When `ddb-tui` needs DDB, resolve in this order:

1. `--ddb-path`;
2. `DDB_BACKEND_PATH`;
3. an executable named `ddb` beside the current `ddb-tui` executable; and
4. `ddb` on `PATH`.

When `ddb tui` needs the frontend, resolve in this order:

1. an internal dispatcher-only `--tui-path` used by packaging tests;
2. `DDB_TUI_PATH`;
3. an executable named `ddb-tui` beside the current `ddb` executable; and
4. `ddb-tui` on `PATH`.

Every candidate must be a regular executable file. Resolution errors list the
locations checked. Automatic download is out of scope.

### 4.4 Compatibility parsing

The DDB CLI refactor must preserve the current positional configuration form.
The intended grammar is conceptually:

```text
ddb [legacy-global-options] [CONF_FILE]
ddb serve [serve-options] [CONF_FILE]
ddb tui [ddb-tui arguments...]
```

Before changing Clap declarations, add parse-only golden tests for all currently
documented DDB command lines. A path literally named `tui` or `serve` can be made
unambiguous with `./tui`, `./serve`, or `--config` and must be documented.

## 5. Architecture

### 5.1 Process and API relationship

```text
User
  |
  | ddb tui debug.yaml
  v
ddb dispatcher
  |
  | exec/spawn ddb-tui --ddb-path <this ddb> debug.yaml
  v
ddb-tui supervisor and terminal UI
  |
  | spawn ddb serve debug.yaml + safe runtime overrides
  v
DDB backend --------------------------------------------------+
  |                                                          |
  | GDB / LLDB / Mock and distributed target discovery       |
  +---------------- public DDB API v2 ------------------------+
                             |
                             v
                     ddb-api-client in TUI
```

On Unix, `ddb tui` should replace itself with `ddb-tui` where practical. On
platforms without equivalent `exec` behavior, it spawns and waits while forwarding
termination and preserving the frontend exit code.

The dispatcher passes its own absolute executable path as `--ddb-path`. Therefore
the managed backend is the same DDB build the user invoked, and the TUI does not
accidentally resolve another installation from `PATH`.

### 5.2 DDB modes

`ddb <config>` keeps the existing interactive stdin and API behavior.

`ddb serve <config>`:

- disables the stdin REPL;
- stays alive based on service/session shutdown policy rather than stdin state;
- applies explicit runtime transport overrides after loading configuration and
  before deployment-policy validation;
- writes a machine-readable startup report;
- drains API streams and debugger sessions on graceful shutdown; and
- returns stable process exit codes for config, bind, debugger, and internal
  failures.

The two modes share `ApplicationRuntime`; `serve` is not a second backend
implementation.

### 5.3 Configuration ownership and runtime overrides

The frontend must not deserialize `Config`, `Conf`, `StaticSessions`, debugger
commands, framework fields, or discovery settings. It performs only:

1. path existence/canonicalization needed to launch the child;
2. secure launcher-runtime setup; and
3. forwarding the path to DDB.

DDB introduces a `RuntimeOverrides` value distinct from the serialized
configuration. Managed overrides include:

```text
api_server_bind = 127.0.0.1
api_server_port = 0
api_auth_token_file = <private runtime token file>
startup_report = <private runtime report path>
managed_parent = <launcher identity or liveness channel>
```

Application order is:

```text
compiled defaults
    < configuration file
    < ordinary DDB CLI overrides
    < managed-launcher safety overrides
```

The effective non-secret configuration is included in debug diagnostics. Token
contents are never included. Managed overrides are visibly reported so a user is
not surprised that a configured `0.0.0.0:5000` listener became an ephemeral
loopback listener.

### 5.4 Startup report protocol

The TUI creates a private runtime directory and passes a startup report path.
DDB writes a temporary sibling file, flushes it, sets restrictive permissions,
and atomically renames it to the requested path.

Ready report shape:

```json
{
  "protocol_version": 1,
  "status": "ready",
  "pid": 12345,
  "endpoint": "http://127.0.0.1:43817",
  "server_instance_id": "opaque-id",
  "api_versions": ["v2"],
  "backend_version": "0.1.15"
}
```

Failure report shape:

```json
{
  "protocol_version": 1,
  "status": "failed",
  "phase": "config_validation",
  "code": "CONFIG_INVALID",
  "message": "StaticSessions[1].binary_path does not exist"
}
```

The report is a launcher protocol, not a debugger API. It contains only startup
coordination metadata. It must be versioned, size-bounded, UTF-8, reject unknown
required protocol versions, and never contain credentials.

The backend writes `ready` only after the listener is bound and basic readiness
can succeed. Session attachment continues asynchronously and is observed through
the public snapshot/event stream.

### 5.5 Managed authentication

For initial loopback HTTP mode:

1. Create a per-run directory in the platform runtime/temp location.
2. Restrict it to the current user (`0700` on Unix).
3. Generate independent random control and administrative tokens using an OS
   cryptographic random source.
4. Write the DDB token document as a regular `0600` file using create-new and
   no-follow semantics where supported.
5. Give the ordinary API client only the control credential.
6. Keep administrative shutdown capability inside the supervisor component.
7. Remove temporary credentials after the backend has exited.

The token path may be present in the backend command line because it is not a
credential, but its parent directory and contents must remain private. The token
value must not be placed in `DDB_API_TOKEN` for a child process.

Unix-domain socket support may be evaluated later, but it is not required for the
first implementation and must not delay the safe loopback workflow.

### 5.6 Supervisor lifecycle

The supervisor owns an explicit state machine:

```text
Resolving -> Starting -> WaitingForReady -> Negotiating -> Running
    |            |              |               |            |
    +------------+--------------+---------------+------------+
                              failure
                                 |
                                 v
                         Stopping -> Stopped
```

Rules:

- A child exit before readiness is a startup failure with captured exit status.
- Timeout causes graceful termination followed by bounded forced termination.
- Normal TUI quit calls the authenticated DDB shutdown operation, waits for drain,
  then terminates only if the deadline expires.
- Terminal restoration is attempted regardless of supervisor failure.
- Backend log readers are drained without deadlocking on full stdout/stderr pipes.
- Process groups or platform job objects prevent debugger grandchildren from
  being accidentally orphaned.
- The supervisor never sends shutdown to a client created by `connect` mode.

Initial managed mode may require the TUI to remain alive. `--keep-backend` lands
only after it can persist credentials safely, print a reconnect command, transfer
ownership cleanly, and define cleanup. Do not implement it as “forget the child
handle and leak the temporary directory.”

### 5.7 Launch and attach shortcuts

Shortcut configuration must remain owned by DDB. Add headless startup forms that
construct the existing in-memory `Config` using backend types:

```bash
ddb serve launch --backend gdb --stop-at-entry -- ./app arg
ddb serve attach --backend gdb --pid 1234
```

`ddb-tui launch` and `ddb-tui attach` translate only their own validated CLI
arguments into these DDB CLI forms; they do not write YAML or import core config
types.

Generated defaults:

- `launch`: unique local session identity, loopback target, `stop_at_entry=true`,
  `auto_shutdown=false`, and kill-on-exit;
- `attach`: PID-derived display identity, loopback target,
  `auto_shutdown=false`, and detach-on-exit; and
- both: selected debugger backend, managed API overrides, and no framework-specific
  behavior unless requested explicitly.

Arguments after `--` are passed as an argument vector. They are never joined into
a shell command and are never evaluated by a shell.

### 5.8 Distributed configuration workflow

`ddb tui distributed.yaml` starts one DDB coordinator locally. The configuration
may describe multiple static sessions, discovery, framework extensions, remote
nodes, or distributed breakpoint behavior. The TUI connects once to the
coordinator's local API.

Readiness is layered:

1. **Process started:** DDB child exists.
2. **Service ready:** API listener and application service can answer.
3. **Topology hydrating:** configured targets are being discovered or attached.
4. **Session ready/stopped/running/failed:** per-target API state.

The startup report covers level 2. Levels 3 and 4 must remain API state so all
community frontends see the same truth. The TUI should show progress and partial
failure instead of waiting indefinitely for an all-target barrier.

### 5.9 Diagnostics and frontend presentation

Before terminal entry, executable/config errors may be printed normally. After
terminal entry, startup and reconnection states use a dedicated status view.

Minimum messages:

- `Starting DDB…`
- `DDB API ready at 127.0.0.1:<ephemeral>`
- `Connecting to 2 configured sessions…`
- `1 ready, 1 attaching`
- actionable backend failure with log path; and
- shutdown progress when it exceeds a short threshold.

Do not stream unrestricted backend logs into the source/output panel. Debuggee
output continues through the public output stream; backend operational logs remain
separate and bounded.

## 6. Planned code shape

Exact module names can change, but responsibilities must remain separated:

```text
ddb/core/src/
  arg.rs                         # legacy, serve, and tui CLI grammar
  main.rs                        # dispatch only
  launcher_dispatch.rs           # locate/exec ddb-tui
  startup/
    mod.rs                       # RuntimeOverrides and startup phases
    report.rs                    # bounded atomic report protocol
    shortcut.rs                  # launch/attach Config construction
  app/
    runtime.rs                   # shared interactive and serve runtime

ddb-tui/src/
  cli.rs                         # managed/connect/launch/attach grammar
  main.rs                        # mode selection and terminal lifecycle
  supervisor/
    mod.rs                       # state machine and owned child handle
    resolve.rs                   # ddb executable resolution
    runtime_dir.rs               # secure temporary files/tokens
    report.rs                    # startup report decoder
    process.rs                   # spawn, log capture, signal, shutdown
  api.rs                         # public SDK adapter only

ddb/core/tests/
  cli_compatibility.rs
  managed_startup.rs

ddb-tui/tests/
  managed_cli.rs
  e2e_mock.rs                    # extend current PTY suite
```

Do not create a shared private crate that allows the TUI to call DDB application
services directly. A small published launcher-protocol type crate is permissible
only if duplication becomes material; a two-struct local codec is preferable at
first because the protocol is deliberately narrow.

## 7. Implementation stages

Each stage must leave the repository buildable and retain existing manual-connect
behavior until managed mode is proven.

### Stage 0: freeze the UX and compatibility contract

#### Rationale

The CLI refactor touches the oldest DDB entry path. Characterization must precede
changes so the new convenience command does not break automation or stdin users.

#### Work

1. Add an ADR recording the two-binary/one-command decision, process ownership,
   config ownership, and why an in-process TUI backend was rejected.
2. Add parse-only golden tests for existing DDB and `ddb-tui` invocations.
3. Define stable launcher error codes, startup report v1, and process exit codes.
4. Document the command matrix and deprecation policy for `ddb-tui --api`.
5. Record current startup and shutdown behavior with Mock, GDB, and LLDB.

#### Exit checks

- All currently documented commands are represented by tests.
- The ADR and CLI help snapshots agree on command ownership.
- No runtime behavior changes in this stage.

### Stage 1: add DDB headless serve and startup primitives

#### Rationale

The frontend cannot safely supervise a backend that treats stdin EOF as lifecycle
control, uses a preselected port, or exposes only human logs for readiness.

#### Work

1. Refactor argument parsing into legacy/default, `serve`, and `tui` branches
   without changing `ddb <config>` semantics.
2. Add `ddb serve <config>` using the existing `ApplicationRuntime` with the REPL
   disabled.
3. Add typed `RuntimeOverrides` and apply them before security validation and
   listener construction.
4. Support port `0` and return the actual bound address from API server startup.
5. Implement bounded atomic startup report v1 for ready and failed states.
6. Ensure config validation and bind failures produce stable phases/codes.
7. Add graceful headless shutdown and deterministic exit status.

#### Exit checks

- `ddb <config>` stdin/API tests remain unchanged and pass.
- `ddb serve <config>` remains alive with closed stdin and exits through API
  shutdown or configured auto-shutdown.
- Fifty parallel Mock starts with port `0` have no port collision.
- Malformed config, unavailable debugger, bind failure, and report-path failure
  are distinguished and leave no child process.

### Stage 2: implement managed configuration mode in ddb-tui

#### Rationale

Configuration-driven startup provides the largest usability improvement and
already covers local and distributed sessions. It should land before shortcut
syntax.

#### Work

1. Add the new CLI grammar while retaining `--api` as connect compatibility.
2. Implement DDB executable resolution and validation.
3. Create the secure runtime directory and scoped token document.
4. Spawn `ddb serve <config>` with loopback/port-zero/auth/report overrides.
5. Implement startup timeout, report decoding, bounded stderr capture, and API
   version/capability negotiation.
6. Hand the connected public SDK client to the unchanged application/UI path.
7. Implement graceful shutdown and cleanup for the owned child.
8. Show managed/connected ownership and backend endpoint/version in diagnostics.

#### Exit checks

- `ddb-tui debug.yaml` starts Mock DDB and reaches an interactive source view with
  one command.
- A multi-session configuration begins rendering available sessions while slower
  sessions are still connecting.
- `ddb-tui connect --api URL` and legacy `ddb-tui --api URL` spawn no process.
- No test or `/proc` inspection can find token values in argv, environment, logs,
  status text, or report files.
- Every failure path restores terminal mode and removes temporary credentials.

### Stage 3: add launch and attach shortcuts

#### Rationale

Most conventional debugging should not require YAML, but shortcut behavior must
still be constructed and validated by DDB.

#### Work

1. Add `ddb serve launch` and `ddb serve attach` constructors over the existing
   backend-neutral configuration/runtime model.
2. Add corresponding `ddb-tui` subcommands that supervise those forms.
3. Validate executable path, PID syntax, backend availability, argument fidelity,
   and lifecycle defaults.
4. Surface Linux ptrace/Yama failures as attach-policy diagnostics without
   suggesting unsafe automatic privilege escalation.
5. Preserve source/debug-symbol guidance in help and errors.

#### Exit checks

- GDB and LLDB launch the unoptimized `real_loop` fixture and stop at entry.
- `▶` moves according to the real top frame after step; `▸` remains independent.
- Breakpoint, continue, stack, locals, evaluate, memory, pause, and step controls
  work through the public API.
- Attach exits by detaching; launch exits by killing, unless explicitly changed.
- Debuggee arguments containing spaces, quotes, and leading hyphens arrive exactly.

### Stage 4: add the `ddb tui` dispatcher

#### Rationale

Once direct managed mode works, the DDB command can expose it without duplicating
or hiding the public process boundary.

#### Work

1. Implement sibling/PATH frontend resolution.
2. Dispatch all remaining arguments unchanged and pass the absolute current DDB
   path as the backend executable.
3. Use process replacement on Unix where supported; implement signal and exit-code
   forwarding elsewhere.
4. Add missing-frontend and incompatible-package diagnostics.
5. Make CLI help present `tui` as the recommended interactive experience and
   `serve` as the headless service form.

#### Exit checks

- `ddb tui debug.yaml` and `ddb-tui --ddb-path <same-ddb> debug.yaml` are
  behaviorally equivalent.
- `ddb tui launch`, `attach`, and `connect` preserve all argument boundaries.
- Only one backend DDB process is created; dispatch cannot recurse into `ddb tui`.
- Exit status and Ctrl-C behavior match direct `ddb-tui` execution.

### Stage 5: harden lifecycle, distributed recovery, and persistence

#### Rationale

The happy path is insufficient for a debugger. A frontend crash or partial remote
failure must not leave users unsure which programs are still controlled.

#### Work

1. Add parent-liveness/process-group or platform-job handling for owned backend
   and debugger descendants.
2. Test graceful drain, forced timeout, backend crash, terminal loss, and TUI
   panic behavior.
3. Add per-session lifecycle policy if the current global `on_exit` cannot safely
   represent mixed launch-and-attach configurations. Preserve the global field as
   a compatibility default.
4. Display partial distributed topology readiness and reconnection without
   conflating it with backend service readiness.
5. Implement `--keep-backend` only with a persistent private session directory,
   ownership transfer, reconnect metadata, explicit cleanup, and documented token
   rotation. Otherwise leave the option unavailable.
6. Add a safe `ddb sessions`/reconnect discovery design only if persistence is
   implemented; do not scan arbitrary processes or temp directories.

#### Exit checks

- The lifecycle fault-injection matrix has no unexpected surviving DDB, debugger,
  or debuggee process.
- Expected surviving attached/persistent debuggees are listed explicitly in test
  assertions and user messages.
- A TUI restart can reconnect to an intentionally persistent backend without
  reissuing mutations.
- Mixed distributed target failure leaves healthy targets controllable.

### Stage 6: package, document, and release

#### Rationale

Sibling resolution and one-command UX are only credible when tested from the same
artifacts users install.

#### Work

1. Produce release archives/packages containing both binaries, licenses,
   completion files, and configuration examples.
2. Add a package manifest containing paired versions and supported API ranges.
3. Smoke-test extracted artifacts in an empty `PATH` using sibling resolution.
4. Document configured, launch, attach, connect, distributed, authentication,
   failure-recovery, and uninstall workflows.
5. Update top-level and TUI READMEs so one-command managed mode is first; retain a
   manual two-terminal section for backend/frontend development.
6. Add upgrade and rollback notes, including mixed-version behavior.

#### Exit checks

- A fresh user can download one artifact and run the Mock example with one command.
- GDB/LLDB prerequisites are diagnosed before attempting a real session.
- Package smoke tests prove `ddb tui`, direct `ddb-tui`, and headless `ddb serve`.
- Documentation never instructs distributed users to start two terminals by
  default.

## 8. Verification matrix

### 8.1 Unit and component tests

- CLI parsing and help snapshots for legacy and new forms.
- Executable resolution order, non-executable rejection, and paths with spaces.
- Runtime override precedence and managed remote-bind suppression.
- Startup report atomicity, size bounds, unknown version, malformed JSON, partial
  write, symlink/no-follow handling, and secret exclusion.
- Secure runtime directory and token-file permissions.
- Supervisor transitions, timeouts, early exit, double-shutdown, and drop safety.
- Structured argument preservation for launch and attach.
- Redaction property tests using known token sentinels.

### 8.2 Process integration tests

| Scenario | Required assertion |
|---|---|
| Legacy `ddb config` | stdin and API behavior unchanged |
| `ddb serve config` with stdin closed | service remains alive |
| Managed Mock config | one command reaches usable TUI |
| Managed two-session config | partial readiness is visible |
| Port-zero burst | no frontend port reservation or collision |
| Invalid config | typed error, child reaped, terminal clean |
| Missing GDB/LLDB | actionable backend-availability error |
| Backend exits during hydration | bounded error and terminal restoration |
| TUI quits | owned backend drains and exits |
| External connect quits | external backend remains alive |
| Launch shortcut | target killed by default |
| Attach shortcut | target detached and remains alive |
| Ctrl-C/SIGTERM | defined policy and no terminal corruption |
| Version mismatch | explicit versions and no unsafe fallback |
| Token sentinel | absent from argv/env/logs/report/UI |

### 8.3 PTY behavior tests

Extend the existing `ddb-tui/tests/e2e_mock.rs` harness instead of creating an
unrelated terminal driver. Test:

- `ddb-tui <mock-config>` managed mode;
- `ddb tui <mock-config>` dispatcher mode;
- GDB and LLDB real fixture launch through the one-command path;
- source execution marker `▶` versus navigation cursor `▸` after exact real steps;
- breakpoint create/enable/disable/delete;
- call stack, locals, evaluation, memory, output, and DDB distributed backtrace;
- mouse controls and keyboard controls;
- config/startup failure before and after alternate-screen entry; and
- normal, interrupt, and crash terminal restoration.

The real fixture must be built with the development profile:

```bash
cargo build --profile dev \
  --manifest-path ddb/core/tests/fixtures/real_loop/Cargo.toml
```

### 8.4 Packaging tests

Create a temporary extraction directory containing only the release artifact.
With a deliberately minimal `PATH`, verify:

```bash
./bin/ddb tui --help
./bin/ddb tui mock-example.yaml
./bin/ddb-tui --ddb-path ./bin/ddb mock-example.yaml
./bin/ddb serve mock-example.yaml
```

Do not use workspace-relative paths in these tests.

### 8.5 Suggested repository gates

From `ddb/`:

```bash
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
tools/check-api-release.sh
cargo test -p ddb --test cli_compatibility
cargo test -p ddb --test managed_startup
```

From `ddb-tui/`:

```bash
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
cargo test --test e2e_mock -- --ignored
```

The exact new test target names may change, but equivalent coverage cannot be
dropped from CI.

## 9. Quality gates

### Functional

- No ordinary local or distributed configured workflow requires two terminals.
- All existing TUI debugger functionality works in managed and connect modes.
- Managed startup never depends on a hard-coded port or sleeps for readiness.
- Target/session state remains API-derived and consistent across first-party and
  community clients.

### Security

- Managed real-debugger workflows use authenticated v2.
- Managed mode cannot expose the API beyond loopback through configuration.
- Credential files are private, short-lived, and redacted everywhere.
- No automatic privilege escalation is attempted for attach failures.
- Fuzz startup report and CLI inputs; run existing API security gates unchanged.

### Reliability

- No known startup, shutdown, timeout, or signal path leaks an owned child.
- No child output pipe can block shutdown.
- Terminal restoration is independently guarded from backend cleanup.
- Repeated start/quit loops and parallel starts pass under sanitizing/process-leak
  instrumentation available in CI.

### Performance

- Measure launcher overhead separately from DDB initialization, debugger startup,
  symbol loading, and TUI hydration.
- Managed orchestration adds no more than 100 ms p95 on the same host beyond the
  equivalent manual DDB start plus TUI connect, excluding build time.
- Control/event latency gates from the API plan remain unchanged because managed
  mode uses the same SDK and transport.
- Startup polling uses filesystem notification or bounded backoff and consumes
  negligible idle CPU.

### Maintainability

- There is one supervisor implementation and one DDB runtime implementation.
- Human logs are never parsed as a protocol.
- TUI code has no DDB core/config dependency.
- Every externally visible CLI or launcher-protocol change has help, tests,
  changelog entry, and compatibility classification.

## 10. Auditable commit and PR plan

Do not mix these stages with unrelated API/domain refactors. Each commit body must
include:

```text
Requirements: UX-xx, UX-yy
Why: <user-visible or architectural reason>
Compatibility: <unchanged/additive/deprecation>
Tests: <exact commands and notable cases>
Security: <credential/bind/lifecycle impact or not applicable>
Rollback: <how this slice can be reverted safely>
```

### PR U0: contract and characterization

1. `docs(ux): add integrated ddb and tui usability plan`
2. `docs(adr): keep two binaries behind one-command workflow`
3. `test(cli): freeze ddb and ddb-tui invocation compatibility`
4. `test(process): characterize backend startup and shutdown`

No behavior change.

### PR U1: DDB headless startup foundation

1. `refactor(cli): add compatible ddb command routing`
2. `feat(ddb): add api-only serve mode`
3. `feat(ddb): add managed runtime overrides and ephemeral bind`
4. `feat(ddb): publish atomic startup reports`
5. `test(ddb): verify headless readiness and startup failures`

Rollback: the new `serve` branch and override/report plumbing can be removed while
the legacy default branch remains unchanged.

### PR U2: TUI managed configuration workflow

1. `refactor(ddb-tui): separate cli modes from terminal runtime`
2. `feat(ddb-tui): resolve and supervise local ddb`
3. `feat(ddb-tui): secure managed api credentials`
4. `feat(ddb-tui): start from ddb configuration`
5. `test(ddb-tui): cover managed startup shutdown and redaction`

Rollback: retain connect mode as the documented path; no backend API rollback is
required.

### PR U3: launch and attach shortcuts

1. `feat(ddb): construct headless launch and attach sessions`
2. `feat(ddb-tui): add launch and attach commands`
3. `test(ddb-tui): exercise real gdb and lldb shortcuts`
4. `docs(debug): document symbols ptrace and lifecycle defaults`

### PR U4: unified DDB entry point

1. `feat(ddb): dispatch tui commands to the companion binary`
2. `test(cli): verify sibling resolution arguments signals and exit codes`
3. `docs(cli): make ddb tui the primary interactive workflow`

### PR U5: lifecycle and distributed hardening

1. `feat(process): supervise backend debugger and debuggee lifetimes`
2. `feat(config): support safe mixed-session exit policies` if required
3. `feat(ddb-tui): show distributed partial readiness and ownership`
4. `test(process): add lifecycle and topology fault injection`
5. `feat(session): add persistent managed ownership` only if its gate passes

Persistence should be a separate PR if it cannot remain reviewable.

### PR U6: packaging and release

1. `build(release): package compatible ddb and ddb-tui binaries`
2. `test(release): smoke test sibling discovery from artifacts`
3. `docs(ux): add configured launch attach connect and recovery guides`
4. `chore(release): publish paired compatibility metadata`

No PR may claim the one-command workflow complete before artifact-level smoke tests
pass.

## 11. Risks and mitigations

| Risk | Mitigation |
|---|---|
| CLI subcommands break positional configs | Parse goldens first; preserve default branch; document `./tui` ambiguity |
| Frontend and backend recurse | Dispatcher always passes absolute DDB path; TUI always invokes `serve`, never `tui` |
| Port allocation race | Backend binds port `0` and reports the actual address |
| TUI duplicates config semantics | Forward config path; construct shortcuts inside DDB |
| Token leakage | File-only credentials, private directory, sentinel/redaction tests |
| TUI crash or terminal loss orphans processes | Owned process group/job, parent liveness, bounded shutdown fallback |
| Distributed target delays block the UI | Report service readiness first; project target progress through API |
| `--keep-backend` leaks secrets/processes | Do not ship until ownership and persistent credential design passes |
| Mixed package versions fail mysteriously | Sibling-first resolution and explicit API/version diagnostics |
| Backend logs overwhelm TUI | Bounded capture plus separate retained log |
| In-process convenience path bypasses API | Architecture/dependency test forbids DDB core imports in TUI |

## 12. Definition of done

This usability work is complete only when all of the following are true:

1. `ddb tui <config>` is the documented primary interactive workflow.
2. Direct `ddb-tui <config>` behaves equivalently.
3. A distributed configuration starts without manual backend orchestration.
4. `launch`, `attach`, and externally owned `connect` modes have distinct and
   tested ownership semantics.
5. DDB and `ddb-tui` remain separately runnable and communicate only through the
   published API.
6. Existing `ddb <config>` and `ddb-tui --api` compatibility tests pass.
7. Managed startup uses port `0`, authenticated loopback, a versioned startup
   report, bounded diagnostics, and no token leakage.
8. Normal exit, startup failure, timeout, Ctrl-C, backend crash, and TUI crash
   have verified process and terminal outcomes.
9. Mock, GDB, LLDB, multi-session/distributed, reconnection, and PTY suites pass.
10. Official package smoke tests prove sibling discovery without workspace or
    `PATH` assumptions.
11. Help, tutorials, security guidance, compatibility metadata, and release notes
    describe the same command contract.
12. The requirement-to-test traceability table and every planned commit/PR record
    the exact verification evidence used.

## 13. Instructions for the implementing agent

1. Start at Stage 0; do not begin by changing `main.rs` argument parsing.
2. Inspect the current dirty working tree and preserve all unrelated API/TUI work.
3. Keep old and new paths working together until managed PTY tests pass.
4. Prefer small application/runtime seams over copying the DDB startup graph.
5. Never solve startup convenience by importing backend internals into `ddb-tui`.
6. Never put token values in CLI arguments, environment, startup reports, or test
   snapshots.
7. Build the real fixture with the development profile when verifying source
   stepping; release optimization of DDB or the TUI is unrelated.
8. Record every test command and retained failure/benchmark artifact in the PR.
9. Stop and write an ADR amendment if implementation requires changing process
   ownership, config ownership, API integrity, or default lifecycle decisions.
10. Do not mark the project complete until release-artifact smoke tests—not only
    workspace Cargo tests—pass.
