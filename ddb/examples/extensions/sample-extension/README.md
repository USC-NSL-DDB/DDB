# DDB sample extension

This crate is deliberately independent of the DDB backend. It implements
`ddb_api_extension::ExtensionProvider`, publishes its schemas, exposes table,
tree, key/value, text, and action presentation descriptors, and implements one
bounded action.

A framework adapter registers the provider as an `Arc<dyn ExtensionProvider>`.
The DDB registry validates it at startup; frontends discover it through
`GetCapabilities` and `GetSnapshot` without extension-specific wire code.
