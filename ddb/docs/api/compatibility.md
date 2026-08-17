# DDB API compatibility and deprecation policy

Status: normative for `ddb.api.v2` beginning with its first published preview.

This policy protects community frontends from accidental wire and semantic
breakage. Protobuf under `proto/ddb/api/v2` is canonical for services and wire
types. `operation_policy.json` is canonical only for transport, permission,
error, and stream metadata that Protobuf cannot express. Checked-in runtime
bindings, descriptors, specifications, and SDK contracts are derived artifacts.

## Compatibility promises

DDB v1 and v2 are separate contracts. Existing v1 routes, JSON shapes, and event
payloads are not silently changed by v2 work. During migration, both versions
run concurrently and clients negotiate v2 through server information and
capabilities.

Within `ddb.api.v2`:

- Existing field numbers, field wire types, oneof membership, enum numbers,
  package names, service names, and method input/output types are immutable
  after release.
- New fields, enum values, messages, and methods may be added. Clients must
  tolerate additions and must not assume an enum is exhaustive.
- Removed fields and enum values are reserved by both name and number. A field
  is never renumbered or reused.
- Existing optional presence is not changed. Absence is not reinterpreted as a
  scalar default.
- Stable messages do not gain `google.protobuf.Any`, `Struct`, or `Value`.
  Schemaless data remains confined to the documented raw-command and extension
  envelopes.
- Public identifiers remain opaque strings. Their current internal spelling is
  not an API guarantee.
- An advertised capability can be added or removed at runtime. Unsupported
  behavior fails with `DDB_ERROR_CODE_UNSUPPORTED`; it never silently falls
  back to a different action.

Buf's `FILE` breaking rules are the minimum mechanical gate. Review also
checks semantic compatibility because a schema can be wire-compatible while
changing meaning.

## Binary Protobuf behavior

Binary Protobuf is the canonical typed wire representation.

- Unknown fields are accepted.
- Unknown numeric enum values remain present in generated integer fields and
  must be handled as unknown.
- `uint64`, `int64`, and `sint64` retain their full range.
- Message order, map iteration order, and unknown-field retention after a
  decode/re-encode cycle are not application semantics.

## ProtoJSON behavior

The required HTTP/JSON binding uses the standard ProtoJSON mapping:

- field names are emitted in lower camel case and parsers also accept original
  proto field names;
- 64-bit integers are emitted as decimal strings;
- bytes are emitted as base64;
- enum values are emitted by their Protobuf names;
- timestamps are UTC and `Z`-normalized;
- durations and field masks use their Protobuf well-known-type mappings; and
- optional scalar presence is preserved.

DDB-generated Rust decoders deliberately ignore additive unknown JSON fields.
An unknown JSON enum name maps to its `UNSPECIFIED` value so an additive
response does not make the entire document unreadable. Application code must
treat `UNSPECIFIED` as unknown, not as a known behavior.

ProtoJSON itself cannot preserve unknown fields through reserialization.
Frontends that proxy messages losslessly should use binary Protobuf or retain
the original JSON document.

## Deprecation lifecycle

A deprecation is published in all of these places:

1. the schema comment and generated API reference;
2. `Capabilities.deprecations`, including a replacement when one exists; and
3. release notes with a removal-not-before release.

A deprecated field remains on the wire and is reserved if production stops
populating it. Released v2 fields and enum values are not physically removed or
reused. A released method is removed only in a new API major package, except
when an urgent security issue makes continued exposure unsafe.

For behaviors outside immutable wire declarations, removal occurs no earlier
than two minor releases and 90 days after announcement, whichever is later.
Security fixes may use a shorter window, but must return a typed error and
publish migration guidance.

## Baselines and checks

The last published v2 descriptor set is the breaking-change baseline. Before the
first v2 publication, the pull-request base branch is the provisional baseline.
After publication, release automation must retain the descriptor artifact and
CI must compare against that released artifact or tag rather than only against
the current branch.

Run the local gates from `ddb/`:

    buf format --diff --exit-code
    buf lint
    buf breaking --against '../.git#tag=<last-api-release>,subdir=ddb'
    cargo run -p ddb-api-codegen -- --check
    cargo test -p ddb-api-types --all-targets
    oasdiff breaking <released-openapi-v2.json> docs/api/generated/openapi-v2.json
    ./tools/check-asyncapi-compatibility.sh <last-api-release>

CI pins oasdiff and the AsyncAPI CLI. Buf owns recursive Protobuf payload
compatibility; the AsyncAPI comparison owns operation, channel, binding, and
DDB stream-policy compatibility. See ADR 0007 for why the latter uses a
payload-free projection with explicit extension severity overrides.

Regenerate only after an intentional schema edit:

    cargo run -p ddb-api-codegen -- generate

A generated diff without the schema or generator change that caused it is not
reviewable and must not be merged.
