# DDB API v2 control/output evidence — 2026-08-14

This directory retains three release-build runs of typed `NEXT` admission to a
replayable stopped event at a later source line. HTTP/ProtoJSON and
gRPC/Protobuf use the same application service. The paired output scenarios
generate the same bounded 2 MiB load per sample; one client drains output and
the other deliberately leaves its response unread.

Every point uses a fresh Mock DDB process, four threads per session, two
warmups, and twelve measured samples. Values below are the median of the three
run percentiles, in milliseconds:

| Scenario | Sessions | p50 | p95 | p99 |
|---|---:|---:|---:|---:|
| HTTP step-to-stop | 1 | 27.224 | 27.499 | 27.542 |
| HTTP step-to-stop | 16 | 28.565 | 28.827 | 28.984 |
| HTTP step-to-stop | 64 | 31.757 | 32.057 | 32.205 |
| gRPC step-to-stop | 1 | 27.078 | 27.447 | 27.573 |
| gRPC step-to-stop | 16 | 28.370 | 28.687 | 28.767 |
| gRPC step-to-stop | 64 | 32.157 | 32.706 | 32.782 |
| HTTP + drained output | 1 | 42.381 | 63.183 | 67.149 |
| HTTP + drained output | 16 | 40.026 | 67.840 | 70.936 |
| HTTP + drained output | 64 | 49.880 | 64.249 | 65.955 |
| HTTP + unread output | 1 | 41.030 | 65.691 | 70.152 |
| HTTP + unread output | 16 | 38.575 | 62.524 | 63.220 |
| HTTP + unread output | 64 | 41.337 | 57.857 | 62.097 |

The slow-consumer comparison is unread output versus actively drained output,
not unread output versus no output. Median p95 changes are +2.508 ms / +4.0%
at one session, -5.316 ms / -7.8% at 16 sessions, and -6.391 ms / -9.9% at 64
sessions. No point exceeds both the roadmap's relative and absolute regression
thresholds. Output parsing/routing itself remains visible when either loaded
scenario is compared with isolated control; this evidence does not claim that
2 MiB of debugger output is free.

The benchmark exposed and led to fixes for two production defects:

- optimized builds had elided thread-status mutation because it was executed
  inside `debug_assert!`; and
- a stalled HTTP body left the shared output broadcast receiver behind. A
  non-blocking per-subscriber pump now drains shared ingress into an
  independently bounded queue and aggregates explicit event/byte gaps.

Files:

- [`environment.md`](environment.md): exact command, host/toolchain, binary
  hashes, and limitations.
- [`run-1.json`](run-1.json): SHA-256
  `d54637a859815025afb61d734578e3dea32c171855d21eaa4ff38d8491caf57d`.
- [`run-2.json`](run-2.json): SHA-256
  `257145993f54fefa929315141ddd10d7f1495e4e979060ed372aa47e67068703`.
- [`run-3.json`](run-3.json): SHA-256
  `10f08dc409d0f97624592d91615b267fc44d253bc6520974e4fc918011e61327`.

This evidence verifies control-to-stop correctness and the bounded
slow-consumer policy for the stated Mock workload. It is not CPU, allocation,
RSS, throughput, complete wire-byte, or real-backend evidence.
