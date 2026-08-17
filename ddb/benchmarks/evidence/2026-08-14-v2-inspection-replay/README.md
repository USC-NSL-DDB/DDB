# DDB API v2 inspection/replay evidence — 2026-08-14

This directory retains three release-build runs of four frontend-critical
public API workflows:

- 10,000 variable roots delivered through bounded SDK page collection;
- 1/16/64 MiB logical memory transfers through advancing, at-most-1 MiB
  `ReadMemory` chunks;
- one typed `NEXT` observed by 1/8/20 concurrent public state subscribers; and
- forced SDK reconnect plus cursor replay convergence at 1/16/64 Mock sessions.

Every point uses a fresh authenticated DDB process, one thread per session, one
warmup, and three measured samples. Each response, stop location, operation,
cursor transition, and final byte/item count is validated. Values below are the
median of the three run percentiles, in milliseconds:

| Scenario | Scale | p50 | p95 | p99 |
|---|---:|---:|---:|---:|
| Variable inspection | 10,000 variables | 363.909 | 367.077 | 367.405 |
| Memory transfer | 1 MiB | 284.808 | 285.799 | 286.065 |
| Memory transfer | 16 MiB | 4,588.557 | 4,639.994 | 4,644.566 |
| Memory transfer | 64 MiB | 17,848.699 | 17,919.814 | 17,926.136 |
| State fanout | 1 subscriber | 26.846 | 26.866 | 26.871 |
| State fanout | 8 subscribers | 27.730 | 27.812 | 27.820 |
| State fanout | 20 subscribers | 28.858 | 29.061 | 29.067 |
| Reconnect/replay | 1 session | 43.996 | 43.998 | 43.998 |
| Reconnect/replay | 16 sessions | 42.001 | 42.034 | 42.039 |
| Reconnect/replay | 64 sessions | 42.956 | 42.986 | 42.989 |

The memory result is a complete ProtoJSON/HTTP workload, including base64
encoding and decoding, so it is not expected to scale like an in-process copy.
The session topology and initial snapshot are established before reconnect
timing starts; that scenario measures forced reconnect, replay subscription,
typed `NEXT`, operation completion, and projection convergence—not process
startup.

Files:

- [`environment.md`](environment.md): exact commands, host/toolchain, binary
  hashes, timing boundaries, and limitations.
- [`run-1.json`](run-1.json): SHA-256
  `a2d841ed42a8568bb1cab2edc14965f1944df17ded90415799ed185319dfb578`.
- [`run-2.json`](run-2.json): SHA-256
  `85a5d93273a5c9b45c25d3d6f6adde6a4edb39f41204baf7c7de90c07dc6b2fd`.
- [`run-3.json`](run-3.json): SHA-256
  `2bce36959ac70f2f5aec04de469526594776edde050d66b088fbc134cb63561e`.

This evidence verifies bounded correctness and latency for the stated Mock
workloads. It is not CPU, allocation, RSS, complete wire-byte, power, or real
GDB/LLDB performance evidence.
