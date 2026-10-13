# DDB source workspaces

Shared filesystem support for DDB source browsing. `SourceRoot` pins an explicitly
granted directory. Reads and listings resolve within that grant, including
symlinks. Call these synchronous methods from a bounded blocking executor.

`SourceCache` shares independent content and directory-snapshot budgets across
roots without sharing their identities or permissions. `read` opens and checks
the file before reusing an immutable document; `Document::window` reads its line
index. `list` always refreshes a directory. `list_cached` permits a short-lived
snapshot for pagination, with access and metadata rechecks.

This crate does not choose exports, debugger ownership, or source mappings. Those
policies belong to callers. See [the API guide](../docs/api/source-workspaces.md)
for the DDB server configuration and contract.

Run the module tests with:

```sh
cargo test --manifest-path ddb/Cargo.toml -p ddb-source-workspace
```
