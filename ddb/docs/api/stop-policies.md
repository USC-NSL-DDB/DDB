# Backend stop policies and logpoints

DDB owns stop classification, peer pauses, and logpoint execution. They run
without a TUI and apply to every connected client. These settings belong to
the backend process and reset on restart.

## Configure shared stop behavior

Call `DebuggerControlService/ConfigureDebugger` with a broadcast target:

```json
{
  "context": {"idempotencyKey": "set-all-stop"},
  "target": {"broadcast": {}},
  "stopPolicy": {"mode": "STOP_MODE_ALL_STOP"},
  "preconditions": {"serverInstanceId": "<observed backend instance>"}
}
```

The backend starts in `STOP_MODE_INDEPENDENT`. `STOP_MODE_ALL_STOP` pauses
running sessions outside the initiating session. `STOP_MODE_GROUP_STOP`
limits those pauses to the initiating session's logical group, including
members that join while the cause remains stopped. Automatic peer interrupts
and secondary threads from a process-wide stop do not establish another cause.

The TUI selects all-stop on a fresh backend. It respects a policy another client
has already configured. Concurrent policy writers can set
`preconditions.resourceVersion` to the singleton policy's identity and revision.
DDB checks that version again when applying the configuration.

Request `SNAPSHOT_SECTION_STOP_POLICY`, or subscribe to state events with
`RESOURCE_KIND_STOP_POLICY`. The singleton contains the mode, generation,
initiating execution state, pending peer targets, and per-target outcomes.
Successful outcomes acknowledge the pause command. The execution-state lane
reports whether each target actually stopped. A failed pause stays visible and
is not retried repeatedly against unchanged peer state. A new cause or a new
running generation can trigger another attempt.

Explicit execution retires pending automation. This includes v2 operations,
legacy API commands, and stdin commands. Breakpoint updates, deletion, native
console commands, and assignments also invalidate pending automatic work.

## Guard execution across clients and restarts

Set `preconditions.serverInstanceId` on mutations to reject a queued request
addressed to an earlier backend process. The Rust SDK's
`client.for_server_instance(instance)` applies that default to mutations and
their retries. The TUI captures the instance when queuing an intent.

For a thread resume action, copy the stopped `ExecutionState` into an
`ExecuteRequest.executionGuard`:

```json
{
  "context": {"idempotencyKey": "continue-observed-stop"},
  "target": {"thread": {"threadId": "<observed thread>"}},
  "action": "EXECUTION_ACTION_CONTINUE",
  "executionGuard": {
    "serverInstanceId": "<observed backend instance>",
    "executionStateId": "<observed execution state>",
    "revision": "<observed revision>"
  }
}
```

Admission checks the public execution state. The session actor then applies
preceding events and atomically claims the corresponding native stop before
dispatch. A later native stop or a second claim fails with
`FAILED_PRECONDITION`, including before a running event arrives. DDB's own
caller-context restoration does not create another native stop. Retrying the
same idempotency key returns the original operation.

A guard requires one explicit thread and supports Continue, Next, StepIn,
StepOut, and Jump. A full transport queue fails the guarded command rather
than delaying it after validation. A dispatched claim remains consumed if
the debugger rejects the command. A client may issue a deliberate unguarded
action or wait for a new native stop.

## Configure logpoints

Set `BreakpointSpec.logMessage` when creating a logical breakpoint, or update
`log_message` through `UpdateBreakpoint`'s field mask. Clear the optional field
to restore an ordinary breakpoint. Templates apply to existing and inherited
members and remain available for a temporary breakpoint's final hit.

`counter={counter}` evaluates `counter`. `{{` and `}}` emit literal braces.
Literal text never becomes a debugger expression. Templates and rendered
messages are limited to 64 KiB, with at most 128 expressions. At most 16
automatic tasks run concurrently, with a 30-second deadline.

DDB evaluates against the originating stopped frame. Its guarded continuation
revalidates and consumes that native stop at dispatch, then publishes one
console output record attributed to its session and thread. Every output
subscriber sees the same record, and thread filters can select logpoint
output. Ordinary debugger streams retain session attribution.

Evaluation errors, output overflow, saturation, cancellation, and superseded
stops never trigger an automatic continuation. A failed logpoint remains
inspectable and establishes an ordinary stop cause under all-stop or
group-stop. Output publication failures also leave the thread stopped. Output
replay retains its existing loss-reporting behavior.

Logpoints interrupt the target and may evaluate debugger expressions with
side effects. They are not non-stopping tracepoints. Peer pause coordination
does not create a simultaneous distributed snapshot.
