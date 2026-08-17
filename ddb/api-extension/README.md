# DDB API extension provider

`ddb-api-extension` is the public, transport-independent provider interface for
adding namespaced DDB state, schemas, actions, events, and generic presentation
hints. It depends only on the public `ddb-api-types` contract; extension crates
must not import DDB core state or transport code.

Implement `ExtensionProvider`, return a stable descriptor plus every referenced
schema, and register the provider through a DDB framework adapter. DDB validates
the descriptor once at startup, bounds state and action payloads, checks scopes,
isolates provider failures, and exposes the result through the same v2 API used
by every frontend.

The complete authoring contract and generic presentation document are in
[`docs/api/extension-authoring.md`](../docs/api/extension-authoring.md). A
self-contained provider with table, tree, key/value, text, and action views is
available in [`examples/extensions/sample-extension`](../examples/extensions/sample-extension).

The first release intentionally uses normal Rust linkage through an integration
adapter. It does not promise a stable Rust dynamic-library ABI or load arbitrary
`.so` files into the debugger process. Independently deployed integrations can
instead use the public DDB API boundary.
