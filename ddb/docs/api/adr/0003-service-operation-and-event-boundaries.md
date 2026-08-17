# ADR 0003: Separate reads, operation admission, events, and admin

Status: accepted

Date: 2026-08-14

## Context

A debugger combines bounded reads, commands that may outlive an HTTP request,
high-volume output, ordered state changes, fanout, and privileged lifecycle
operations. Mirroring every backend command as an RPC would leak MI dialects
and make transports disagree.

## Decision

V2 has four cohesive public services:

- `DebuggerService` provides metadata and bounded reads.
- `DebuggerControlService` admits typed mutations.
- `DdbEventService` exposes independent state and output streams.
- `DdbAdminService` isolates health, readiness, and privileged shutdown.

Every admitted mutation returns the same `OperationAdmissionResponse`.
Operations carry a typed lifecycle, result/error, per-target outcomes,
idempotency reference, and related state cursor. The shared response is
intentional: SDKs implement admission, retries, polling, and event correlation
once. It is a documented exception to Buf's unique per-RPC response style.

State events stream their `StateEvent` envelope directly, and output streams
their `OutputEvent` envelope directly. An additional RPC response wrapper
would add wire and client noise without an independent semantic layer.

All services are ports over one `DdbApplicationService`. Transport adapters
must not read runtime managers or submit backend commands directly. Control is
unary and streams are server-to-client; ordinary frontends do not require a
bidirectional RPC.

## Consequences

Command admission is distinct from later running/stopped state. Long-running
and fanout work is observable without holding an HTTP request open.
Backpressure and replay policy can differ between state and output without
starving control transitions.

The raw-command RPC remains an explicitly unstable parity escape hatch, not the
template for new typed methods.
