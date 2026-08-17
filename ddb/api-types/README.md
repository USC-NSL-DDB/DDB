# ddb-api-types

Generated Rust messages for the public `ddb.api.v2` contract.

The canonical schema lives in `proto/ddb/api/v2` in the
[DDB repository](https://github.com/USC-NSL-DDB/DDB). This crate contains
transport-neutral Protobuf messages, ProtoJSON support, well-known types, and
the checked descriptor set. It does not depend on the DDB backend.

```rust
use ddb_api_types::v2;

let request = v2::ListSessionsRequest::default();
assert!(request.context.is_none());
```

All public IDs are opaque strings. Consumers must not parse or perform
arithmetic on their current spelling. See the repository's
`docs/api/compatibility.md` before persisting or proxying messages.
