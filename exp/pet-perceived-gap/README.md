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

`baseline` is a genuine control, not a simulation: DDB's gdb extension checks for
the `FAKETIME` environment variable and, finding none, skips the correction
entirely (`check_faketime_present` in `ddb/core/assets/gdb_ext/runtime-gdb.py`).
The pause train, the binary and the workload are otherwise identical.

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

## Requirements

Everything runs on **one machine**. There is no cluster.

- **Passwordless SSH to `127.0.0.1`.** DDB attaches gdb over SSH even when the
  target is on the same host — there is no local short-circuit. Check with
  `ssh 127.0.0.1 true`; if it prompts, `ssh-copy-id 127.0.0.1`.
- **Passwordless `sudo`.** These machines run `kernel.yama.ptrace_scope=1`, so a
  non-root gdb may only attach to its own descendants. The app under test is not
  a descendant of gdb, so gdb must be root (`Conf.sudo: true` in the config).
- **Docker**, for the EMQX broker DDB starts for service discovery. If you are in
  the `docker` group but have not started a new login shell, the harness detects
  that and re-execs through `sg docker` for you.
- `gcc`/`g++`, `cargo`, and `libpaho-mqtt3c` (the DDB connector's MQTT client).
  `../../scripts/setup.sh` installs paho if you do not have it.

The scripts check all of these and fail with the fix rather than hanging.

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

Numbers move a little between machines and runs — the residual has come out
between 2.6 and 3.2 ms across ours — but the two orders of magnitude between the
modes do not.

### Where the ~3 ms residual comes from

It is not noise, and it is worth knowing before you quote the number.

In `sync_pause_time` (`ddb/core/assets/gdb_ext/runtime-gdb.py`) the pause duration
is timestamped **first**, and the new offset is written into the debuggee
**afterwards** — and that write goes one character at a time, each character a
separate `gdb.execute("set *(char*)…")` round-trip into the inferior:

```python
curr_ts_ns = time.perf_counter_ns()               # (1) pause measured up to here
accumulated_time = pause_duration_s + accumulated_time
modify_env_variable("FAKETIME", f"-{accumulated_time}")   # (2) ~25 pokes, process still stopped
... on_finish()                                   # (3) -exec-continue
```

Everything between (1) and (3) is time the process spends stopped that the offset
does not account for, so the application perceives it. Timing that write directly
against a stopped inferior gives **~1.2–1.4 ms** for a 22-character value on a
local gdb; over DDB's SSH-driven gdb it is larger, which is most of the residual.

So the residual is dominated by the cost of *applying* the correction, not by any
inaccuracy in *measuring* the pause. Two obvious ways to shrink it — writing the
whole string in one `gdb.execute` instead of one poke per character, and
timestamping after the write rather than before — would change the mechanism the
paper is describing, so this harness deliberately measures it as it stands rather
than quietly improving it.

## A bug this experiment depends on

`runtime-gdb.py` is executed by **gdb's embedded Python**, whose version comes
from how gdb was built — a stock gdb on Ubuntu 20.04 embeds **Python 3.8**.
Several signatures in it used `X | None` (3.10+) and `dict[str, object]` (3.9+),
which are evaluated at `def` time and raise:

```
TypeError: unsupported operand type(s) for |: 'type' and 'NoneType'
```

gdb prints that as a console traceback and carries on, so the failure is quiet:
**none** of the extension's MI commands get registered, and DDB's calls to
`-exec-interrupt-if-running` and `-record-time-and-continue` come back as
`Undefined MI command`. Pause-time compensation is silently disabled and the
debuggee sees the full pause — indistinguishable, from the outside, from the
mechanism simply not working.

Fixed by adding `from __future__ import annotations` to `runtime-gdb.py` (PEP 563
— annotations are never evaluated), which restores Python 3.8 compatibility.
`ddb` embeds the extension at compile time, so **rebuild `ddb` after touching
it**; `build_all.sh` does.

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

## Notes for anyone editing this

- **The zero padding in `FAKETIME=-00000000000000000` is load-bearing.** DDB
  rewrites the offset *in place* inside the debuggee's environ block and cannot
  grow the allocation, so the initial value has to be at least as long as the
  longest value that will ever be written over it.
- **`FAKETIME_NO_CACHE=1` is the other half of "dynamic".** By default libfaketime
  re-reads `FAKETIME` only every 10 s; without this the app runs on a stale offset
  for the first several pauses and the measured gap is an artifact of the cache.
- **The pause train waits for the process to actually stop.** An MI command written
  to DDB's fifo is queued and applied asynchronously. An earlier version of this
  harness sent `-exec-interrupt`, slept 100 ms, and sent
  `-record-time-and-continue` — and the process turned out to have been stopped
  for only ~75 µs, because both commands were still in the queue together and gdb
  applied them back to back. `run_experiment.sh` polls `/proc/<pid>/stat` for state
  `t` before it starts counting.
- **`Framework: grpc`, not omitted.** The field defaults to `nu`, whose adapter
  expects a Caladan runtime a plain pthreads binary does not have, and a
  misspelling silently becomes `Unspecified`, which panics. `grpc` is the generic
  C++ path: same MQTT discovery, and it sources `runtime-gdb.py`, which is what
  defines `-record-time-and-continue` in the first place.
- **libfaketime must be the copy in this repo.** DDB's fixes to it (futex handling,
  immature wakeups out of `nanosleep`/`clock_nanosleep`, `pthread_cond_clockwait`
  interposition) are in no upstream release.
