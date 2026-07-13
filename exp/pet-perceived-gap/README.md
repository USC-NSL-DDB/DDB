# PET — Perceived Time Gap (single node)

Measures how much wall-clock time an application still **perceives** across a
debugger pause, once DDB compensates for the pause with a dynamically adjusted
`libfaketime` offset.

When a debugger stops a process, real time keeps running but the process does not.
On resume the application would normally see the whole pause arrive at once as a
single enormous jump in the wall clock — enough to fire every timeout, blow up
rate limiters, and make a debugged distributed system tear itself apart.

DDB hides the pause. `libfaketime` is `LD_PRELOAD`ed into the application and
subtracts an offset from every clock call; DDB's gdb extension grows that offset
by the exact duration of each pause, **writing the new value straight into the
debuggee's `environ` block**, before it resumes the process. The offset is
*dynamic*: it is rewritten on every resume.

This experiment quantifies what is left over.

| Mode | The application under test runs… | Expected result |
|------|----------------------------------|-----------------|
| `faketime` | under `libfaketime`, with DDB adjusting the offset on every resume | the pause is (almost) invisible to the app |
| `baseline` | with no `libfaketime` at all | the app perceives the **whole** pause |


## What is actually measured

The application runs a busy compute loop and, once per iteration (~36 µs), records
a pair of timestamps:

| | | |
|---|---|---|
| `perceived_us` | `CLOCK_REALTIME`, via `std::chrono::system_clock` | What the app **thinks** the time is. `libfaketime` intercepts this call and subtracts DDB's offset, so this is the timeline the application actually experiences. |
| `real_us` | `rdtsc`, via `RealTimer` | Ground truth. `libfaketime` cannot intercept a CPU instruction, so this keeps advancing while the process is stopped. |

A pause shows up as a jump in `real_us` far larger than the sampling interval:
real time ran on while the app was frozen and produced no samples. The question is
what `perceived_us` did across that same jump:

```
perceived gap  =  perceived_us[k] - perceived_us[k-1]  -  (one sampling interval)
```

That residual is the result. Without faketime it is the whole pause. With
faketime and a dynamic offset it should be close to zero.

Because the app times its own run with `rdtsc`, the run lasts a fixed amount of
*real* time regardless of how much time the offset hides.

## Run it

```bash
./build_all.sh          # libfaketime + ddb + the app + matplotlib
./run_experiment.sh --mode both
```

`--mode both` runs the control and the treatment back to back and prints the
comparison. `--mode faketime` / `--mode baseline` run just one.

Results land in `results/`: one CSV of samples per run, plus `perceived_gap.png`
and `.pdf`. To re-analyse existing CSVs without re-running:

```bash
python3 analyze.py --pause-ms 100 results/baseline_*.csv results/faketime_*.csv
```

If something is wedged (a `kill -9`, a crash), `./stop_all.sh` clears out gdb, the
app, ddb and the EMQX container.

### Knobs

All are environment variables; the defaults are what the numbers below used.

| | default | |
|---|---|---|
| `DURATION_SEC` | `30` | how long the app runs, in real seconds |
| `NUM_PAUSES` | `20` | how many times to stop it |
| `PAUSE_MS` | `100` | how long to hold it stopped each time |
| `GAP_MS` | `500` | how long to let it run in between |
| `WORK_SIZE` | `50000` | inner loop length; sets the ~36 µs sampling interval |

```bash
DURATION_SEC=60 NUM_PAUSES=40 PAUSE_MS=250 ./run_experiment.sh --mode both
```

## Expected result

`./run_experiment.sh --mode both` at the defaults, on the CloudLab machine this
ships with:

```
mode        pauses   pause (ms)    perceived gap (ms)   hidden
                         median     median        p95   median
--------------------------------------------------------------------------
baseline        20       103.27    103.272    103.521     0.0%
faketime        20       106.36      3.128      3.500    97.1%
```

Without correction the application perceives the entire ~103 ms pause. With
faketime and a dynamically adjusted offset it perceives **~3 ms**, i.e. DDB hides
**97%** of the pause. `perceived_gap.png` shows the accumulated offset climbing
one tread per pause, against a flat baseline.


## Files

| | |
|---|---|
| `common.sh` | shared config, preconditions, cleanup |
| `build_all.sh` | libfaketime → ddb → the app → matplotlib |
| `start_ddb.sh` | renders the config, starts ddb on a fifo, waits for the broker |
| `run_experiment.sh` | the driver: attach, resume, inject the pause train, collect |
| `analyze.py` | samples → per-pause gap, summary table, figure |
| `stop_all.sh` | kill everything, remove the EMQX container |
| `src/`, `include/` | the app under test (`faketime_pause`) and its rdtsc timer |
| `ddb/pet_config.yaml.tmpl` | the DDB config; the generated copy is gitignored |
