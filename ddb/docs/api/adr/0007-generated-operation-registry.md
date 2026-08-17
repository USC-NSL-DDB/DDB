# ADR 0007: generate runtime bindings and public specifications from one operation registry

Status: accepted

## Context

Protobuf is the canonical DDB v2 type and service contract, but it does not
express every property of the HTTP/ProtoJSON and NDJSON bindings. In particular,
it does not define DDB authorization scopes, the exact `DdbError` HTTP mapping,
request limits, stream replay and ordering guarantees, heartbeats, independent
backpressure lanes, or loss signaling.

Previously, the generator reconstructed some of that information in Rust while
the Axum router, error mapping, and stream constants were maintained separately.
Deterministic generation detected stale checked-in files, but could still
reproduce a specification that disagreed with the running server.

Google HTTP annotations can describe ordinary RPC paths and request bodies, but
they do not cover DDB's authorization or stream semantics. The v2 HTTP path is
also a deterministic function of the fully qualified RPC name, so repeating all
43 paths as annotations would add another surface that could drift.

## Decision

DDB builds one validated operation registry from two canonical inputs:

1. Protobuf descriptors own services, methods, request and response types,
   streaming classification, field semantics, and source comments.
2. `proto/ddb/api/v2/operation_policy.json` owns only metadata Protobuf cannot
   express: HTTP conventions and limits, authorization overrides, exact error
   statuses, and DDB stream semantics.

Code generation fails unless the policy service and stream sets exactly match
the descriptors, every error enum value has one mapping, operation paths are
unique, and supported transport invariants hold. The resulting registry emits:

- the Axum v2 route and authorization table, error status mapping, request
  limit, content type, and heartbeat constants used by the running server;
- OpenAPI, AsyncAPI, and the review-oriented operation registry document; and
- Rust, gRPC, TypeScript, and Python contract artifacts and method tables.

The generator performs bidirectional set and identity checks between the
registry and both public specifications. Runtime authorization tests enumerate
the same registry rather than a copied route list.

## Quality gates

Checked-in artifacts must reproduce byte-for-byte. Redocly and the AsyncAPI CLI
validate the generated standards documents. JSON Schema 2020-12 tests validate
every embedded example plus captured live unary success, typed error, operation,
and stream-event payloads. Buf, oasdiff, and the AsyncAPI CLI compare Protobuf,
OpenAPI, and stream-topology/semantics compatibility respectively.

The AsyncAPI diff library requires dereferenced inputs and cannot finitely
dereference DDB's recursive Protobuf value graph. The compatibility script
therefore removes payload schemas before invoking the official diff. Buf remains
authoritative for those payload types; the AsyncAPI diff remains authoritative
for operations, channels, messages, bindings, and DDB stream extensions. The
normal, unmodified AsyncAPI document is still standards-validated and its live
payloads are still schema-validated.

## Consequences

Adding an RPC requires a Protobuf declaration and, only when applicable, a
policy scope override or stream policy. A contributor cannot add a documented
route without adding the runtime route, or change runtime authorization and
stream constants without changing the generated specifications.

The policy file and registry builder remain DDB-owned code because the missing
semantics are DDB-specific. Their scope is deliberately narrow and reviewable;
standard validators and compatibility tools test the projections instead of
claiming that deterministic generation alone proves conformance.
