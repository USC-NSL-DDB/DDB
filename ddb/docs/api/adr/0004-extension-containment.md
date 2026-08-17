# ADR 0004: Dynamic extension data is contained

Status: accepted

Date: 2026-08-14

## Context

DDB frameworks and community integrations need to add actions, state,
presentation hints, and events without requiring a core release for every
experiment. Unbounded dynamic values in core resources would weaken generated
SDKs, compatibility review, validation, and security limits.

## Decision

Core debugger resources and broadly useful DDB behavior remain strongly typed.

An extension declares a stable namespaced ID, owner, version, schemas and hashes,
required scopes, actions, event types, compatibility range, and presentation
descriptors. Its payload is one bounded `ExtensionPayload` containing bytes or
one complete JSON value plus schema and media-type metadata.

The server validates descriptor existence, permission scope, media type, schema
metadata, and advertised size/depth limits before dispatch. Extension failures
cannot mutate core projections outside the application-service transaction or
prevent core snapshot/event delivery.

`DynamicValue` is allowed only for raw-command compatibility and explicitly
documented extension paths. Stable messages do not use
`google.protobuf.Any`, `Struct`, or `Value`; a descriptor test enforces
that boundary.

## Consequences

Community integrations can evolve independently and frontends can discover
generic table, tree, key/value, text, and action presentations. Frontends that
understand a declared schema may offer richer views.

Extension payload semantics are owned by their namespace, not by the DDB core
compatibility promise. Features that become common and stable should graduate
to typed v2 messages through the normal additive review process.
