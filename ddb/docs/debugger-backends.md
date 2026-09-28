# Debugger backends

DDB separates its runtime model from debugger-native protocols. GDB/MI remains
the compatible user command dialect, but MI values, parsing, token framing, and
bootstrap commands do not leak into command-flow or state code.

```text
DDB command plan
      |
      v
session runtime ---- DebuggerProtocol ---- native transport
      |                                      |
      v                                      v
neutral records                       GDB/MI or LLDB JSON
      |
      v
event reducers and domain services
```

## Backend boundaries

`DebuggerBackend` owns static and lifecycle behavior:

- backend identity and capability validation
- bundled runtime assets
- debugger process startup
- attach or launch bootstrap
- interrupt, signal delivery, console, framework-action, and shutdown commands
- construction of the per-session protocol codec

Startup is an explicit two-phase plan. A backend may first write the minimal
native prelude needed to install its structured protocol; the session then
waits for protocol readiness and submits every attach, launch, configuration,
and framework action as a normal token-correlated command. Any structured
`error` result aborts activation with the debugger's diagnostic.

`DebuggerProtocol` owns the stateful wire boundary:

- command token and thread framing
- buffering fragmented or coalesced stdout
- parsing native results, events, and stream output
- normalization into `ProtocolRecord`, `Dict`, and `Value`

Each session owns one codec. Protocol buffers are never global or shared across
sessions.

`FrameworkPlugin` describes semantic requirements such as loading the core
runtime or sending a post-start signal. It does not render GDB or LLDB syntax.
The selected backend validates and renders those requirements.

The LLDB backend uses a bundled Python bridge inside LLDB's supported embedded
interpreter. The bridge is a native adapter, not a second runtime model: it
maps the DDB command dialect to `SBTarget`, `SBProcess`, `SBThread`, and
`SBFrame`, then emits prefixed JSON records. Rust remains the owner of routing,
global identities, state transitions, and distributed-backtrace traversal.

Each LLDB session uses an unpredictable channel id in its record prefix. A
record is accepted only when that exact prefix begins the stdout line; inferior
output containing the static marker or another session's prefix remains stream
output. The bridge emits readiness through the same session-bound framing.

## Configuration

Select LLDB with:

```yaml
Conf:
  Debugger:
    backend: lldb
```

Static attach and binary-launch sessions use the same configuration shape for
GDB and LLDB. New configuration uses `PrerunDebuggerCommands` and
`PostrunDebuggerCommands`. The legacy `PrerunGdbCommands` and
`PostrunGdbCommands` keys remain accepted when reading existing files.

LLDB eagerly warms at most 64 stack frames after the first stop for each
process. LLDB otherwise charges its one-time unwind and symbol-cache
construction to the first backtrace request. The stop event is flushed before
the warmup runs, warmup failures are non-fatal, and later stops reuse LLDB's
caches. Startup-sensitive deployments can disable this latency optimization:

```yaml
Conf:
  Debugger:
    backend: lldb
    eager_stack_warmup: false
```

Disabling it preserves stack contents but makes the first stack or remote
metadata request substantially slower on debug-info-heavy binaries.

Debugger commands and scripts are backend-native. A script configured under
`Plugin.DebuggerScripts` must therefore be valid for the selected backend.

## Supported behavior

| Behavior | GDB | LLDB | Mock |
| --- | --- | --- | --- |
| Local binary launch and PID attach | Yes | Yes | Deterministic fixture |
| Threads, processes, sources, frames, registers | Yes | Yes | Fixture subset |
| Breakpoints and execution control | Yes | Yes | Fixture subset |
| Console commands and expression evaluation | Yes | Yes | Fixture subset |
| Frame-filter custom command | Yes | Yes | No |
| Signal listing and delivery | Yes | Yes | Fixture subset |
| Pause-time/`FAKETIME` execution commands | Yes | Yes | No |
| gRPC/Nu remote backtrace and context switching | Yes | Yes | Yes |
| Proclet migration and heap restoration | Yes | No; rejected at startup | No |
| Service Weaver remote backtrace extraction | Yes | No; rejected at startup | Yes |

Unsupported capability combinations fail during backend resolution, before the
application runtime starts. They must not degrade into partial sessions or
silently change command semantics.

## Signal delivery contract

The public `DebuggerControlService.Execute` action `SIGNAL`, legacy
`-send-signal`, and framework startup signals all use
`DebuggerBackend::signal_command(&DebuggerSignal)`. `DebuggerSignal` validates
one signal token, so framework plugins and user requests cannot inject console
commands. Signal names and numbers are interpreted for the target platform.

The operation delivers a signal and resumes the stopped process, subject to
its debugger's signal stop/pass policy. It does not promise that the process
will remain running: the debugger may stop on that signal, and SIGKILL ends the
process. A thread target selects its owning session. Command flow interrupts
a running process first and holds one session lease across interrupt and
delivery so another command cannot interleave. It restores any saved execution
registers left by distributed-stack inspection before delivering the signal,
using the same restoration step as Continue. A failed interrupt or restoration
aborts delivery.
The post-attach startup handshake already has a stopped process. Unsupported signals or delivery mechanisms must return errors, never
silently continue without delivering the signal.

Framework plugins declare `DebuggerBootstrapAction::Signal`, without command
syntax. Its shared implementation validates the token and delegates to the same
backend method as an ordinary signal request. A new backend must implement that
method explicitly, including returning an error for unsupported delivery.
Generic command flow must not render `signal ...` or another debugger's syntax.
Raw console commands remain backend-native and are a separate interface.

GDB renders its native `signal` command. LLDB renders the bridge's
`-exec-signal` operation. On Linux, LLDB's native asynchronous signal path rejects
stopped processes. Its bridge queues an OS signal on the inferior's host before
resuming. This also works for DDB's SSH sessions because the bridge runs on the
target host. The bridge rejects stopped non-host LLDB platforms, where a local
PID could refer to a different process. This platform-specific mechanism stays
inside the LLDB implementation, without changes to connectors or framework
plugins.

For the gRPC/Nu startup handshake, the connector consumes signal 40 through
`sigwait` and raises SIGTRAP for the initial pause. Signal 40 is a framework
requirement, not a special case in command flow. The LLDB signal implementation
also handles other signals. GDB's `signal 0` convention is backend-specific and
is not supported by LLDB; portable clients use Continue to resume normally.

The real API suite exercises the same typed signal request on GDB and LLDB for
both stopped and running targets. Distributed-stack tests verify that nonfatal
signal delivery restores a reconstructed caller first, on both backends.
LLDB bootstrap tests additionally cover named
signals and the real greeter suite checks the signal-40 connector handshake.

## Pause-time and FAKETIME contract

The `-record-time-and-continue`, `-record-time-and-next`,
`-record-time-and-step`, and `-record-time-and-finish` commands compensate the
inferior's realtime clock for time spent stopped in either debugger. GDB and
LLDB follow the same rules:

- If the inferior has no `FAKETIME` environment entry, the command resumes
  normally without clock synchronization.
- If `FAKETIME` is present, the inferior must also contain the exact entry
  `FAKETIME_NO_CACHE=1`. libfaketime otherwise caches its configuration for up
  to ten seconds and does not observe the debugger's memory update promptly.
- The existing `FAKETIME` entry must be long enough for the accumulated
  negative offset. A debugger can overwrite environment storage, but it cannot
  safely grow that storage in place.
- Synchronization uses one bounded memory write followed by read-after-write
  verification. The accumulated pause time is committed only after verification
  succeeds.
- A configured-but-invalid target returns an error and remains stopped. The
  debugger never resumes with a stale or partially written clock offset.

For local binary launch, when DDB itself is started with `FAKETIME`, both
backends replace the launched inferior's value with a fixed-width initial value
and add `FAKETIME_NO_CACHE=1` before the process starts. Loading libfaketime is
still the caller's responsibility, normally through `LD_PRELOAD`.

Attach mode cannot change libfaketime's constructor-time cache policy. Start the
target with a writable fixed-width value and no-cache mode before attaching:

```bash
LD_PRELOAD=/path/to/libfaketimeMT.so.1 \
FAKETIME=-00000000000000000000.000000000 \
FAKETIME_NO_CACHE=1 \
FAKETIME_DONT_FAKE_MONOTONIC=1 \
FAKETIME_DISABLE_SHM=1 \
./application
```

`FAKETIME_DONT_FAKE_MONOTONIC=1` keeps duration and sleep clocks monotonic;
`FAKETIME_DISABLE_SHM=1` prevents independent test or service processes from
sharing libfaketime state.

The real integration suite verifies this contract against the inferior's actual
`SystemTime` under both backends. It covers launch, attach, every record-time
resume action, repeated accumulation, undersized buffers, missing no-cache
configuration, and the fail-closed no-resume behavior.

## Adding another backend

1. Add its configuration variant and backend module under `core/src/debugger/`.
2. Implement `DebuggerBackend`, including a truthful capability declaration
   and fail-fast validation for unsupported framework requirements. Implement the
   signal-delivery contract above rather than relying on a GDB console fallback.
3. Implement a per-session `DebuggerProtocol`. Keep native parser types inside
   the backend module and normalize at this boundary.
4. Render all bootstrap and shutdown behavior in the backend. Do not add native
   command branches to `SessionProcess`, framework plugins, reducers, or state.
5. Map the DDB command vocabulary used by command-flow services. Unknown
   pass-through commands must return an explicit backend error.
6. Add codec tests for fragmentation, coalescing, malformed input, scalar
   normalization, and maximum record size.
   Shared-output protocols must also cover strict record boundaries,
   per-session channel isolation, and explicit readiness.
7. Add real integration coverage for launch/attach, breakpoint lifecycle,
   source queries, custom commands, clean shutdown, and distributed backtraces
   where the backend advertises support.
8. Run the correctness gates in `runtime-architecture.md` and compare the same
   release benchmark scenarios before and after hot-path changes.

## Performance constraints

Protocol codecs buffer bytes once per session and parse only complete records.
Application payloads move as owned neutral values; compatibility rendering is
deferred until presentation. Native debugger noise is classified as stream
output instead of triggering parser retries. Backend shutdown must close its
transport promptly rather than consuming the generic forced-close timeout.

Distributed-backtrace latency is measured after sessions are stopped and ready.
For LLDB this means the default one-time stack warmup is session-readiness work,
not command work. Benchmark startup/readiness separately when changing the
warmup policy; do not treat moving work across that boundary as eliminating it.

Do not add a second parse/serialize cycle between `DebuggerProtocol` and
command flow. If a backend requires a bridge, its structured output should map
directly to the neutral record schema.
