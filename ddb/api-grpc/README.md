# ddb-api-grpc

Generated Tonic client and server stubs for the public `ddb.api.v2` contract.

This crate shares all request, response, resource, error, and event messages
with `ddb-api-types`; it contains transport bindings only and has no dependency
on DDB core.

The DDB gRPC listener is currently an opt-in preview. HTTP/ProtoJSON remains
the required stable binding until representative frontend benchmarks and the
transport decision ADR are complete. Preview clients must discover the exact
gRPC endpoint from `ServerInfo` rather than assuming a port.
