# DDB/ddb-tui integrated workflow 0.1 release notes

Status: release candidate; not eligible for official open-source publication until
a project `LICENSE` file is selected and added.

## Highlights

- `ddb tui CONFIG` and direct `ddb-tui CONFIG` provide the same one-command,
  authenticated managed debugger workflow for local and distributed sessions.
- `ddb tui launch -- PROGRAM` and `ddb tui attach --pid PID` add conventional
  debugger shortcuts without bypassing DDB configuration/runtime ownership.
- `ddb serve CONFIG` is a first-class headless API service that does not depend on
  stdin and reports typed, atomic startup readiness/failure.
- The TUI is DDB-native: distributed backtrace is the primary stack and refreshes
  automatically on selection/stops, with inspectable owner frames, explicit
  cross-session boundaries, and persistent truncation diagnostics.
- DDB group/session/thread topology drives capability-gated execution scopes.
- Distributed breakpoints support server-resolved broadcast, inheritable groups,
  individual sessions, explicit multi-target sets, and aggregate/sub-breakpoint
  state.
- Typed lazy variables, registers, memory, signals, generic extension panels and
  actions, mouse input, keyboard input, and distinct execution/cursor markers are
  first-class UI surfaces.
- Paired deterministic archives contain both binaries, Bash completion, examples,
  docs, checksums, and explicit API/schema compatibility metadata.

## Lifecycle and security

Managed mode forces authenticated loopback port `0`. Distinct random control and
admin tokens never appear in argv, environment, logs, startup reports, or UI
state. DDB loads then unlinks the credential document before readiness. Normal
quit, Ctrl-C, SIGINT, SIGTERM, SIGHUP/terminal loss, startup timeout, early child
exit, and backend crash have bounded cleanup paths. Unix managed children use a
process group and Linux parent-death signal. Launch kills its generated target;
attach detaches and preserves the original process. Configured sessions support
an optional per-session `on_exit` override with the global field as fallback.

## Compatibility

- DDB: 0.1.15
- ddb-tui: 0.1.0
- Managed API: v2 over HTTP/ProtoJSON
- Schema range: `>=2.0.0, <3.0.0`
- Existing `ddb CONFIG`, `ddb-tui --api URL`, and no-argument local connect remain
  compatible.
- V1 fallback remains explicit and external-connect-only.
- gRPC remains an optional preview and is not a managed-mode dependency.

Version/identity/schema mismatches fail closed and show both versions, supported
and discovered API ranges, and paired-package remediation. Upgrade and roll back
both binaries together using versioned extraction directories; see the user guide.

## Deliberately not included

`--keep-backend` and automatic session discovery are not shipped. Persistent
ownership requires durable private credentials, rotation, reconnect metadata,
and explicit cleanup, and will be a separate design/PR if implemented. Managed
mode does not expose remote API listeners or alter ptrace/host security policy.

The TUI does not relabel `SIGINT` as session termination and does not synthesize
reverse execution, operation cancellation, variable mutation, function/address
breakpoints, or ignore-count mutation when DDB exposes no typed advertised
contract for them. Raw MI remains an explicit backend-specific escape hatch,
not a portable DDB frontend API.

## Verification

The release candidate is gated by workspace/unit tests, fifty concurrent port-zero
starts, structured startup failures, Mock/GDB/LLDB PTY workflows, launch/attach
lifecycle checks, distributed partial readiness, signal/crash/token fault tests,
public API security/conformance, an isolated dispatcher p95 gate, reproducible
archive construction, and empty-`PATH` extracted-artifact smoke tests. Exact
commands and requirement mappings are retained in the integrated usability
readiness report.
