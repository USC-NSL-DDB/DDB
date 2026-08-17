# `ddb tui` dispatcher overhead evidence

Date: 2026-08-15
Gate: managed launcher orchestration adds at most 100 ms p95 on the same host.
Result: PASS.

## Method

The measurement isolates only the thin `ddb tui` dispatch path. A private executable
that exits immediately stands in for `ddb-tui`; this excludes DDB initialization,
debugger startup, symbol loading, API negotiation, and UI hydration. After 10 warmup
pairs, the script alternates 100 direct frontend executions with 100 executions
through the release `ddb tui` dispatcher. It sorts elapsed monotonic wall-clock
samples, selects rank 95, and subtracts the direct-exec p95 from dispatcher p95.

Command:

```bash
ddb/tools/measure-tui-dispatch.sh \
  ddb/target/release/ddb \
  ddb/benchmarks/evidence/2026-08-15-tui-dispatch/result.json \
  100
```

## Result

- Direct placeholder p95: 2.927 ms
- ddb tui dispatch p95: 8.737 ms
- Isolated overhead p95: 5.810 ms
- Required maximum: 100 ms

The raw aggregate result is retained in `result.json`; host/toolchain details are
in `environment.md`. This gate intentionally does not replace the API control,
output, inspection, or replay latency evidence, because managed mode uses those
same public transports after startup.
