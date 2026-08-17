# ADR 0002: Public identifiers are opaque strings

Status: accepted

Date: 2026-08-14

## Context

Current DDB internals frequently use numeric session, group, and thread IDs.
Exposing those widths and allocation rules would prevent composite,
backend-qualified, persistent, or non-numeric identifiers later. Debugger
addresses and source positions also have target-specific semantics.

## Decision

Every public v2 identity is an opaque string, including session, group, process,
thread, breakpoint, sub-breakpoint, operation, extension, server-instance, and
subscription identities. Clients may compare, display when appropriate, and
store IDs, but may not parse or calculate with them.

The canonical `Target` oneof is the only routing selector. It represents
session, thread, group, current selection, sets, fanout, broadcast, first, and
operation targets without exposing the internal router enum.

Addresses remain strings. Memory is bytes. Source lines and columns are
one-based unsigned values, with zero documented only where it means unknown.
Optional scalar presence is explicit. Timestamps and durations use Protobuf
well-known types. Stable enums use an `UNSPECIFIED` zero value.

## Consequences

Initial adapters may stringify existing numeric IDs, but that spelling is not a
contract. Conversion and validation occur at the application-service boundary.

JSON emits 64-bit integers as strings and bytes as base64 according to
ProtoJSON. UI-specific hexadecimal rendering and local source-path mapping stay
in SDK/frontend code.
