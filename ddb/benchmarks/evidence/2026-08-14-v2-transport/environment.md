# DDB v2 transport benchmark environment

Captured: 2026-08-14T18:16:59+00:00

This evidence was collected from the dirty implementation worktree at Git
revision `845846460fac30aa8614430f82753e86ab664b71`. The worktree had 94
modified or untracked paths, so the binary hashes below—not the Git revision
alone—identify the measured implementation.

## Host

- Linux `sapphire2`, kernel `6.8.0-111-generic`, x86_64
- Intel Xeon Gold 5420+, 1 socket, 28 cores, 56 hardware threads
- CPU frequency range: 800–4100 MHz; dynamic scaling was enabled
- Memory: 263,603,780 KiB

## Toolchain

- rustc 1.89.0 (`29483883eed69d5fb4db01964cdf2af4d86e9cb2`), LLVM 20.1.7
- cargo 1.89.0 (`c24e10642`, 2025-06-23)
- GNU GDB 14.2
- LLDB 18.1.3
- libprotoc 26.1
- Buf was not installed locally; CI pins Buf 1.72.0

## Measured binaries

- `target/release/ddb`: `3e9f4ba5c1e7eae2834398ea5306091d3fe2943fab033a06574a46d1e1ca19f2`
- `target/release/ddb-bench`: `dbd53f4f662d49106362f2ea5bd6a07589f57d6edea4c9aec24aa3285593653e`

DDB was built with `--release --features grpc-preview`. The release profile
uses thin LTO and one codegen unit. OpenTelemetry export was disabled. Each
transport used a fresh Mock-backed DDB process, bearer authentication, four
threads per session, a reused client connection, five warmups, and thirty
recorded samples. Both transports requested the same seven snapshot sections
and validated identical session/thread counts, cursor presence, and capability
presence on every sample.

## Reproduction

```bash
cargo build -p ddb --release --features grpc-preview
cargo build -p ddb-bench --release

for run in 1 2 3; do
  ./target/release/ddb-bench \
    --binary ./target/release/ddb \
    --scenarios v2-http-snapshot,v2-grpc-snapshot \
    --scales 1,16,64 \
    --threads-per-session 4 \
    --samples 30 \
    --warmup 5 \
    --format json \
    --output "benchmarks/evidence/2026-08-14-v2-transport/run-${run}.json"
done
```

No CPU affinity, fixed-frequency governor, or host isolation was applied.
Interpret small sub-millisecond differences accordingly. These runs measure
end-to-end snapshot latency only; they do not claim CPU, allocation, RSS,
throughput, or complete on-the-wire byte measurements.
