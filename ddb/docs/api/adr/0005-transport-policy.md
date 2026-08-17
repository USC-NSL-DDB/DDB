# ADR 0005: HTTP is mandatory and Tonic gRPC remains an optional preview

Status: accepted for the API v2 preview

Date: 2026-08-14

## Context

Protobuf is DDB's canonical v2 contract, but that does not make one Protobuf
transport the right default for every frontend. Browser clients, scripts, and
community tools need a universally usable binding, while native clients may
benefit from binary framing. A second production transport is worthwhile only
when it preserves the same semantics and earns its operational and dependency
cost on representative DDB workloads.

Three release-build runs compared the same full seven-section Mock-backed
snapshot through HTTP/ProtoJSON and Tonic gRPC/Protobuf. Each point used a fresh
DDB process, bearer authentication, a reused connection, four threads per
session, five warmups, and thirty samples. The values below are the median of
the three run percentiles, in milliseconds:

| Sessions | Transport | p50 | p95 | p99 |
|---:|---|---:|---:|---:|
| 1 | HTTP/ProtoJSON | 0.212 | 0.232 | 0.240 |
| 1 | gRPC/Protobuf | 0.184 | 0.197 | 0.198 |
| 16 | HTTP/ProtoJSON | 0.645 | 0.730 | 0.757 |
| 16 | gRPC/Protobuf | 0.529 | 0.618 | 0.655 |
| 64 | HTTP/ProtoJSON | 1.910 | 2.240 | 2.313 |
| 64 | gRPC/Protobuf | 1.582 | 1.912 | 1.952 |

The gRPC p95 was approximately 15% lower at each scale, with an absolute
advantage between 0.034 ms and 0.328 ms. This does not cross the roadmap's 20%
promotion threshold. The evidence also does not measure CPU, allocations, RSS,
throughput, complete wire bytes, streaming, large memory/variable payloads, or
mixed control and bulk traffic. It therefore supports keeping the preview, but
not making it mandatory or preferred for all frontends.

Connect Rust is now an official ConnectRPC project. Its project documentation
reports full Connect, gRPC, and gRPC-Web conformance and Rust 1.88 as its MSRV.
It is also explicitly pre-1.0 and uses Buffa rather than DDB's existing
Prost-generated public messages. Adopting it today would add a second Protobuf
runtime and conversion boundary before DDB has evidence that a multi-protocol
server improves a target workload. Upstream protocol conformance is valuable,
but it does not replace DDB's application-semantic, authentication, replay,
shutdown, and SDK conformance gates.

Primary evaluation sources:

- [Connect Rust project](https://github.com/connectrpc/connect-rust)
- [Connect RFC 007](https://connectrpc.com/docs/governance/rfc/007-rust-implementation/)

## Decision

1. HTTP/ProtoJSON plus the independent replayable state and output streams is
   the mandatory, default, and browser-compatible API v2 binding.
2. Tonic gRPC/Protobuf remains available only through the non-default
   `grpc-preview` feature and a distinct loopback listener. It calls the same
   `DdbApplicationService`, uses the same authorization policy, and must remain
   semantically conformant with HTTP.
3. DDB does not add Connect Rust to the production dependency graph now. This
   is a deferral, not a permanent rejection.
4. A frontend must choose transports from advertised capabilities and must not
   require gRPC. The high-level SDK semantics remain transport independent.

The raw benchmark files, environment, binary hashes, and exact reproduction
command are retained under
[`benchmarks/evidence/2026-08-14-v2-transport`](../../../benchmarks/evidence/2026-08-14-v2-transport/README.md).

## Revisit criteria

Reconsider promotion of gRPC or adoption of Connect only when one comparison:

- exercises identical snapshot, control-to-stop, event replay, output, large
  variable, and chunked memory workloads at the planned scale points;
- records at least three release-build runs plus CPU, allocation, RSS,
  throughput, and wire-byte evidence;
- passes the same DDB conformance, authorization, limit, slow-consumer, and
  graceful-shutdown suites;
- demonstrates the roadmap's material performance threshold or a substantial
  operational/browser simplification; and
- for Connect, uses a stable-enough API/MSRV policy and either consumes DDB's
  public Prost messages directly or justifies and bounds the conversion cost.

## Consequences

Community frontends retain a low-friction interoperable baseline. Native Rust
clients may opt into a modestly faster binding without fragmenting semantics.
DDB carries only one optional native server implementation today, and avoids a
premature second Protobuf runtime. The preview label remains visible until a
broader workload and resource-cost comparison supports a stronger claim.
