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
- interrupt, console, framework-action, and shutdown commands
- construction of the per-session protocol codec

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

## Adding another backend

1. Add its configuration variant and backend module under `core/src/debugger/`.
2. Implement `DebuggerBackend`, including a truthful capability declaration
   and fail-fast validation for unsupported framework requirements.
3. Implement a per-session `DebuggerProtocol`. Keep native parser types inside
   the backend module and normalize at this boundary.
4. Render all bootstrap and shutdown behavior in the backend. Do not add native
   command branches to `SessionProcess`, framework plugins, reducers, or state.
5. Map the DDB command vocabulary used by command-flow services. Unknown
   pass-through commands must return an explicit backend error.
6. Add codec tests for fragmentation, coalescing, malformed input, scalar
   normalization, and maximum record size.
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
