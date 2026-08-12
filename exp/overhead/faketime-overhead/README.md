# libfaketime — Time-API Interposition Overhead

Measures the per-call latency of the time APIs libfaketime interposes —
`gettimeofday(2)`, `clock_gettime(CLOCK_REALTIME)` and
`clock_gettime(CLOCK_MONOTONIC)` — with and without interposition:

| Config | Environment | What it is |
|---|---|---|
| `none` | (nothing) | the glibc/vDSO fast path — the baseline |
| `faketime` | `LD_PRELOAD=libfaketime.so.1 FAKETIME="+0"` | interposed by libfaketime |

The offset is `+0` — the experiment isolates what interception itself costs, independent of any particular fake time.

This is a **single-node** experiment; run it anywhere. The libfaketime under
test is the DDB-shipped copy in this repo (`libfaketime/`), the same `.so` the
PET experiment preloads.

## Run it

```bash
./build_all.sh        # libfaketime + the bench binary
./run_benchmark.sh    # benchmark both configs
```

Each run writes `results/faketime_overhead_<timestamp>.txt` and prints a
summary table. `--cpu N` pins the benchmark to another core (default 2).

## Reference numbers from this cluster

CloudLab Utah, Xeon @ ~3GHz, medians of 15 rounds, 2026-07-11 (two runs agreed
within ~1%):

| API | none | faketime | 
|---|---|---|
| `gettimeofday` | 27.8 ns | 106.3 ns |
| `clock_gettime(REALTIME)` | 27.5 ns | 102.7 ns |
| `clock_gettime(MONOTONIC)` | 27.3 ns | 101.0 ns |

How to read this:

* Interception replaces a ~27ns vDSO call with a PLT-indirected wrapper that
  calls the real function and applies offset arithmetic: ~+75ns per call.
* The *relative* cost (≈3.8×) sounds large but the *absolute* cost is ~75ns
  per call. An application pays it in proportion to how often it reads the
  clock: at 10k time calls/sec that is ~0.08% of one core; even at 1M
  calls/sec it is ~7.5% of one core, not of the whole application.
* All three APIs cost the same to intercept — libfaketime funnels them through
  the same internal path.

## Files

| File | Purpose |
|---|---|
| `bench_time.c` | the measurement binary (calibrated rdtsc, loop-averaged) |
| `build_all.sh` | builds libfaketime (repo copy) + `bench_time` |
| `run_benchmark.sh` | runs both configs, verifies interposition, prints the table |
