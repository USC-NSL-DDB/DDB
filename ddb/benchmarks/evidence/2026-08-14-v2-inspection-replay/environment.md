# Environment and reproduction

Captured on 2026-08-14 UTC.

- Base Git commit: `845846460fac30aa8614430f82753e86ab664b71`
- Source state: dirty implementation worktree; the binary hashes below bind
  the evidence to the exact built artifacts pending an authorized commit.
- Host: Linux `sapphire2`, kernel `6.8.0-111-generic`, x86_64
- CPU: Intel Xeon Gold 5420+, 1 socket, 28 cores, 56 logical CPUs
- Rust: `rustc 1.89.0 (29483883e 2025-08-04)`
- Cargo: `cargo 1.89.0 (c24e10642 2025-06-23)`
- GDB: GNU gdb 14.2 (recorded for environment completeness; this matrix uses Mock)
- LLDB: 18.1.3 (recorded for environment completeness; this matrix uses Mock)
- DDB release binary SHA-256:
  `9c5ef8a04a565d6e3345729b4329814d585b06ac4f212b8dfe949df0081d66ea`
- Release benchmark harness SHA-256:
  `550e00ebff4215796fcc5d9824b21e41f7eeecfc9b34ce69749c9820131ab404`

Build and run command from `ddb/`:

```bash
cargo build -p ddb --release --features grpc-preview
cargo build -p ddb-bench --release

target/release/ddb-bench \
  --binary target/release/ddb \
  --scenarios v2-http-variable-inspection,v2-http-memory-transfer,v2-http-state-fanout,v2-http-reconnect-replay \
  --inspection-variables 10000 \
  --variables-per-frame 500 \
  --memory-sizes-mib 1,16,64 \
  --memory-chunk-bytes 1048576 \
  --state-subscribers 1,8,20 \
  --scales 1,16,64 \
  --threads-per-session 1 \
  --samples 3 \
  --warmup 1 \
  --timeout-ms 15000 \
  --format json \
  --output benchmarks/evidence/2026-08-14-v2-inspection-replay/run-N.json
```

Variable timing begins after handshake, snapshot, frame, and scope selection.
Each 500-root collection uses public SDK pagination with 200-root pages. The
harness also validates arbitrary non-divisible totals using a bounded final
prefix loop without weakening the SDK's collection-size guard.

Memory timing begins after handshake and target selection. Each call advances
the address by the bytes already delivered, validates exact data length and
zero unreadable bytes, and never requests more than the recorded chunk bound.

State-fanout timing starts before typed `Execute(NEXT)` and ends only when all
subscribers agree on a later stopped source line. Reconnect timing starts after
the initial full snapshot; `force_reconnect` drops the stream while preserving
the acknowledged cursor, and the sample ends only after operation completion
and replayed projection convergence.

Limitations: no CPU, allocation, RSS, throughput, complete wire-byte, power,
or real GDB/LLDB measurements were captured. Scheduling was not pinned and the
host was not isolated. Use these files as bounded workflow evidence, not as a
transport-promotion claim.
