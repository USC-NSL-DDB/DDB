# Authoring DDB API extensions

DDB extensions add discoverable, namespaced state and actions without adding
untyped fields to core debugger resources. An extension provider depends only
on `ddb-api-extension` and `ddb-api-types`; it does not receive Axum/Tonic
objects, bearer credentials, or direct access to DDB's mutable runtime model.

The complete independent example is
[`examples/extensions/sample-extension`](../../examples/extensions/sample-extension).
It is also used by the backend service tests and the reference TUI renderer, so
it detects drift at both sides of the public boundary.

## Provider lifecycle

Implement `ExtensionProvider` with four responsibilities:

1. `descriptor()` returns one stable, reverse-DNS-style extension identity and
   declares schemas, permissions, actions, events, compatibility, and generic
   presentation hints.
2. `schemas()` returns every schema referenced by the descriptor. DDB validates
   URI uniqueness, media type, size, JSON syntax where applicable, and the root
   schema's SHA-256 hash at startup.
3. `state()` returns detached `ExtensionPayload` values. It must not expose a
   borrowed lock or mutate DDB core state. One provider failure omits only that
   provider's state.
4. `invoke()` handles a validated action asynchronously and returns a bounded
   payload matching its declared response schema.

A framework integration registers providers as `Arc<dyn ExtensionProvider>`
through `FrameworkPlugin::api_extensions`. The initial public surface uses
normal Rust linkage. DDB deliberately does not load arbitrary Rust dynamic
libraries: Rust has no stable plugin ABI, and a provider loaded into-process has
the backend's privileges. A separately deployed integration should use the
public API rather than relying on a `.so` ABI.

## Descriptor and schema rules

- `extension_id` is a stable project-qualified ASCII name containing a dot,
  such as `org.example.workers`. Renaming it creates a new extension.
- Action, presentation, and column IDs are stable local ASCII identifiers.
- Extension state currently requires exactly `READ`. Each action independently
  requires `CONTROL` or `ADMIN`.
- `minimum_api_version` is currently `v2`; `maximum_api_version` is omitted or
  `v2`.
- Schema identifiers are absolute, whitespace-free URIs. A URN is valid.
- `schema_hash` is the lowercase SHA-256 digest of the root schema document.
- Every request, response, and event schema referenced by the descriptor must
  be registered by the same provider. Schema URIs are globally unique in one
  DDB process.

Frontends obtain descriptors from `GetCapabilities` and fetch documents with
`GetExtensionSchema(extension_id, schema_uri)`. The response includes the
document bytes, media type, and a SHA-256 digest, allowing safe caching and
integrity checks without inflating every capability response.

The registry's compile-time limits are public constants in
`ddb-api-extension`. At this release they are 64 providers, 64 schemas per
provider, 1 MiB per schema, 32 actions, 64 event declarations, 32
presentations, 32 table columns, and 16 state payloads. JSON is limited to depth
64 and 10,000 nodes. The effective payload-byte limit is deployment data and
must be read from `Capabilities.limits.max_extension_payload_bytes`.

DDB validates envelope identity, version, schema URI, media type, byte count,
and JSON shape. It does not implement each extension's business schema. The
provider remains responsible for semantic JSON Schema validation and for
returning `ProviderErrorKind::InvalidRequest` on invalid action data.

## Generic presentation document

The descriptor vocabulary is intentionally limited to `TABLE`, `TREE`,
`KEY_VALUE`, `TEXT`, and `ACTION`. It is data, not executable UI. A generic
frontend may ignore an unknown kind or malformed entry and continue rendering
core debugger state.

For the built-in generic renderer, a JSON state payload uses this version-one
shape:

```json
{
  "presentations": {
    "table_id": {"rows": [["cell 1", "cell 2"]]},
    "summary_id": {"entries": [{"key": "workers", "value": "2"}]},
    "tree_id": {
      "nodes": [
        {"label": "root", "value": "optional", "children": []}
      ]
    },
    "text_id": {"text": "provider ready"},
    "action_id": {"enabled": true}
  }
}
```

The keys match presentation IDs in the descriptor. Table cells and key/value
values may be JSON scalars or containers; generic clients render non-strings as
compact JSON. Table rows, key/value entries, and flattened tree rows are capped
by the frontend as well as the server payload bound. Tree depth is capped.
Extension-specific frontends may use the registered schema for richer views,
but cannot require other clients to understand it.

The `panels` array accepted by `ddb-tui` is a one-way v1 compatibility path for
old built-ins. New providers must use `presentations`.

## Actions, events, and state changes

An action is invoked through `InvokeExtensionAction` with a canonical debugger
target and a client-generated idempotency key. DDB checks the descriptor's
dynamic permission, validates request metadata and bounds, resolves the target,
and returns the normal `Accepted` operation record. Provider execution then
produces `Running` and `Completed` or `Failed` operation events. Provider error
messages are not exposed; clients receive stable sanitized error categories.

The descriptor's `idempotent` flag describes extension semantics. Regardless
of that flag, DDB requires an idempotency key and deduplicates an identical
retry during operation retention. Authors should make actions genuinely safe
under one admitted invocation and must not perform unbounded work. Provider
execution currently has a 30-second server bound.

Changed state is observed by DDB's detached projection bridge and published as
revisioned `EXTENSION_STATE_CHANGED` upserts. Declared extension event types
reserve and validate names/schema ownership; they do not bypass the core state
journal. A future event-emission API must remain bounded and replay-safe.

## Compatibility and testing

Add fields and schemas compatibly. Do not change the meaning of an existing
ID, schema URI, or action. If payload semantics break, publish a new schema URI
and extension version, and constrain the API compatibility range if necessary.

Run at minimum:

```bash
cargo test -p ddb-api-extension --all-targets
cargo clippy -p ddb-api-extension --all-targets --no-deps -- -D warnings
cargo test -p ddb-sample-extension --all-targets
```

Frontend authors should discover extensions dynamically, fetch schemas only
when needed, enforce the advertised bounds, hide actions for insufficient
scope, and ignore unknown extensions without treating them as a connection
failure.
