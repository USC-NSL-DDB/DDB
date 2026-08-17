# DDB API v2 transport evidence — 2026-08-14

This directory retains the three raw release-build runs used by
[`ADR 0005`](../../../docs/api/adr/0005-transport-policy.md). The benchmark
compares the same full public snapshot through HTTP/ProtoJSON and Tonic
gRPC/Protobuf. It is evidence for the current preview decision, not a general
serialization or throughput claim.

Median-of-run percentiles, in milliseconds:

| Sessions | Transport | p50 | p95 | p99 |
|---:|---|---:|---:|---:|
| 1 | HTTP/ProtoJSON | 0.212 | 0.232 | 0.240 |
| 1 | gRPC/Protobuf | 0.184 | 0.197 | 0.198 |
| 16 | HTTP/ProtoJSON | 0.645 | 0.730 | 0.757 |
| 16 | gRPC/Protobuf | 0.529 | 0.618 | 0.655 |
| 64 | HTTP/ProtoJSON | 1.910 | 2.240 | 2.313 |
| 64 | gRPC/Protobuf | 1.582 | 1.912 | 1.952 |

Files:

- [`environment.md`](environment.md): host, toolchain, binary hashes,
  limitations, and exact reproduction command.
- [`run-1.json`](run-1.json): SHA-256
  `1966ec11a1758bccbc31635ffd50852039a665d86f2ca1b5a37d98855b03423f`.
- [`run-2.json`](run-2.json): SHA-256
  `0879ff596bd3f3cf4a208cb8ca74db7b1cf11594ac5e251b2c157eecca698c07`.
- [`run-3.json`](run-3.json): SHA-256
  `f78027631094357115d38fb455fef545bf4ba25dfc1dbbaaff225b24493049c8`.

The gRPC p95 advantage is approximately 15% at all three scales and does not
cross the 20% transport-promotion threshold. Only snapshot latency was
measured. CPU, allocation, RSS, throughput, complete wire-byte, stream, bulk,
and mixed-workload conclusions require new evidence.
