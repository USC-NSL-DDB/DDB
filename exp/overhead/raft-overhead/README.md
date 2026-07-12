# Raft — DDB Overhead Experiment (distributed)

Measures the runtime overhead a debugger imposes on a **3-node Raft cluster**,
under three conditions:

| Mode | What is attached to the three `raft_node` processes |
|------|----------------------------------------------------|
| `none` | nothing — the baseline |
| `ddb`  | **DDB**, which discovers all three nodes and attaches gdb to each |
| `gdb`  | a bare **gdb** driven over MI (the same interface DDB uses), one per node |

The overhead is the difference in achieved throughput between the modes **at the
same offered load**. Everything else — the binary, its flags, the load
generator, the request count — is identical across the three, so the debugger is
the only variable.

## Getting the raft-lab repo (read this first)

Unlike the `nu` and `sw` overhead experiments, the code under test is **not part
of this repository**. It lives in a **private** repo:

```
https://github.com/USC-NSL/raft-lab-cpp-solution.git
```

**You will need to request read access.** Ask the DDB authors and we will grant
your GitHub account permission. Once you have it:

```bash
git clone https://github.com/USC-NSL/raft-lab-cpp-solution.git \
    /mnt/local/raft-lab-cpp-solution
```

The harness defaults to `/mnt/local/raft-lab-cpp-solution` and **errors out with
this instruction if it is missing**. If you keep it elsewhere, point every script
at it with `RAFT_DIR`:

```bash
RAFT_DIR=/path/to/raft-lab-cpp-solution ./build_all.sh
```

> **We can also do this for you.** If you would rather not deal with the access
> request, give us access to your CloudLab cluster and we will clone and build it
> on your nodes ourselves.

Nothing in this harness modifies the raft-lab sources: it builds in-tree at
`build/` (which raft-lab's own `.gitignore` already ignores) and checks out the
`spdlog`/`googletest` submodules, which is exactly what raft-lab's own
`setup.sh` does. A fresh clone still behaves exactly as its README describes, and
`git status` in it stays clean.

## Topology

4 nodes, on the `10.10.1.x` experiment network. **Run every command from node0.**

| Node | IP | Role |
|------|-----|------|
| node0 | `10.10.1.1` | `tput_remote` (the load generator) + DDB + the EMQX broker |
| node1 | `10.10.1.2` | `raft_node`, raft id 1 |
| node2 | `10.10.1.3` | `raft_node`, raft id 2 |
| node3 | `10.10.1.4` | `raft_node`, raft id 3 |

One raft process per machine — three in total — so consensus traffic is genuinely
cross-node. Each node is alone on its machine, so they all share the same two
ports (raft `:50051`, tester `:55001`).

### Why this harness and not raft-lab's `exp/start_cluster.sh`

raft-lab ships `exp/start_cluster.sh`, but it starts **all three nodes on one
machine** (same IP, three ports). Two things had to change for a distributed run,
which is why this directory has its own scripts rather than calling theirs:

* **`--ddb_host_ip` is the debuggee's *own* address** — it is what the connector
  reports to the broker and what DDB ssh'es back into to attach gdb.
  `start_cluster.sh` defaults it to the *client's* IP, which is only correct when
  everything is on one host. Here each node passes its own IP.
* **`report_ready()` is a fire-and-forget RPC with no retry.** If the client's
  ctrl server is not already listening when a `raft_node` starts, that node is
  never seen and the client waits for it forever. So the load generator must go up
  **before** the raft nodes — `run_benchmark.sh` enforces that ordering.

## Prerequisites

- 4 nodes, passwordless SSH + `sudo` from node0 to the others
- `gdb` on node1–3 (`setup_nodes.sh` checks)
- Docker on node0 only (`--mode ddb` runs DDB's managed EMQX broker in a
  container), with your user in the `docker` group — run `newgrp docker` if
  `docker info` fails
- **Free space on `/`.** DDB's broker state and service-discovery config live
  under `/tmp`, and journald grows quickly under benchmark churn; on these 16G
  root disks a full `/` kills DDB mid-run (the harness detects this and rejects
  the run). `sudo journalctl --vacuum-size=100M` reclaims space safely

## One-time setup

```bash
# Toolchain, the DDB-patched gRPC, the DDB connector headers, ddb itself,
# and a Release build of raft_node + tput_remote.
# First run takes ~30 min (gRPC dominates).
./build_all.sh

# Ship the binary + sources to node1..node3 (there is no shared filesystem).
# Re-run this after every ./build_all.sh.
./setup_nodes.sh
```

Two things `build_all.sh` does deliberately, both of which are worth knowing:

* **It builds `Release` (`-O3 -g`), not raft-lab's default `Debug` (`-O0`).** An
  unoptimised binary would understate every mode and make the comparison
  meaningless. `-g` is kept so gdb and DDB have symbols to attach to.
* **It installs gRPC to `/mnt/local/opt/raft-deps`, not `$HOME/.local`.** The root
  filesystem on these images is 16G and gRPC needs ~3.5G. raft-lab's CMake
  hard-codes `$HOME/.local`, so the harness overrides `CMAKE_PREFIX_PATH` on the
  command line — no raft-lab file is touched.

## Measure the overhead

Each invocation brings the cluster up, runs the load generator once, and tears
everything down. **One run = one data point.**

```bash
./run_benchmark.sh --mode none    # baseline, no debugger
./run_benchmark.sh --mode ddb     # DDB attached to all 3 raft nodes
./run_benchmark.sh --mode gdb     # bare gdb/MI debugging all 3 raft nodes
```

Runs vary a little between trials, so for a real number take several of each and
compare the medians:

```bash
for i in 1 2 3; do
  for m in none ddb gdb; do ./run_benchmark.sh --mode "$m"; done
done
```

`./stop_all.sh` cleans up by hand if a run is interrupted.

### Choosing the load (`--clients`)

`--clients` defaults to **1024**, this cluster's knee, where debugger overhead
is most visible. It is the single most important knob, because this is a
**closed-loop** benchmark — each client thread waits for its proposal to commit
before sending the next, so throughput = clients ÷ latency — and the visible
overhead changes dramatically with the operating point:

* **Below the knee** (≲256 clients) the benchmark **cannot detect overhead at
  all**. Raft only replicates on the leader's heartbeat
  (`heartbeat_timeout = 80` ms), so latency is pinned at ~85 ms regardless of
  the debugger, and the servers sit nearly idle — a debugger could cost 2× CPU
  and the number would not move. Measured at 64 clients: none 0.752, gdb 0.751,
  ddb 0.751 Kops/s. Indistinguishable.
* **At the knee** (~1024) the debuggers' added per-proposal latency (a roughly
  constant ~30 ms once loaded) is largest *relative to* the ~112 ms baseline —
  the relative throughput loss peaks here (~20%).
* **On the plateau** (2048+, ~10–11 Kops/s capacity) the baseline is already
  queue-dominated (~180 ms), so the same added latency costs relatively less
  (~13–17%). Past 2048, throughput is flat and latency only queues (4096: same
  tput, 2× latency). Nothing is CPU-saturated even there — the ceiling is
  serialization inside the raft leader, a property of the raft-lab
  implementation, not the harness.

Baseline-only sweep for reference (`--reqs 200 --rounds 1`): 64 → 0.75, 256 →
2.87, 512 → 5.33, 1024 → 8.67, 2048 → 10.10, 4096 → 10.10 Kops/s.

To sweep the load with all three modes yourself:

```bash
for c in 64 512 1024 2048; do
  for m in none ddb gdb; do ./run_benchmark.sh --mode "$m" --clients "$c"; done
done
```

## Read the results

Each run writes `results/c<clients>_r<reqs>_n3_{none,ddb,gdb}_<timestamp>.txt`:

```
# mode=ddb nodes=3 clients=2048 reqs=200 rounds=2
# round latAvg_ms latP50_ms latP90_ms latP99_ms tput_kops
1 192.4 191.8 210.2 232.7 10.09
2 192.1 191.5 209.8 231.9 10.11
avg_tput_kops 10.10
avg_lat_ms 192.2
p99_lat_ms 232.3
```

The number to compare between modes is **`avg_tput_kops`** (thousands of
ops/sec). Side by side:

```bash
grep -H avg_tput_kops results/c2048_r200_n3_*.txt
```

### Reference numbers from this cluster

Medians over 2–3 trials per cell, measured 2026-07-11 on a CloudLab Utah 4-node
cluster (`--reqs 200 --rounds 2`):

| `--clients` | `none` (Kops/s) | `gdb`/MI | vs baseline | `ddb` | vs baseline |
|---|---|---|---|---|---|
| 64 | 0.752 | 0.751 | −0.1% | 0.751 | −0.1% |
| **1024** (default) | **8.67** | **6.96** | **−19.8%** | **6.91** | **−20.3%** |
| 2048 (plateau) | 11.11 | 9.69 | −12.8% | 9.19 | −17.3% |

The headline: **an attached gdb/MI debugger costs this workload ~20% of its
throughput at the knee, and DDB costs essentially the same — marginally more.**
Both debuggers add a roughly constant ~30 ms to the average proposal latency
once the system is loaded; the closed loop turns that into a throughput loss
whose relative size peaks at the knee and shrinks again both below it
(heartbeat-bound: invisible) and above it (queue-dominated: diluted).

Baseline trials have the widest spread (e.g. 10.4–11.1 at 2048), so treat the
percentages as ±3pp; the debugged modes are far more repeatable (gdb at 2048:
9.679/9.687/9.694).

### Where the overhead comes from (verified by decomposition)

Two candidate mechanisms were isolated:

* **DDB's per-RPC backtrace metadata is free here.** With `--ddb` the patched
  gRPC captures caller context and sends it as a `bt_meta` header on every
  outgoing unary RPC (`client_unary_call.h`, gated on `DDB::Initialized()`).
  Running the cluster with the connector initialized but **no debugger attached**
  (`--ddb --wait_for_attach=false`, no broker) measured **11.0 Kops/s** — at the
  top of the baseline range. The propagation mechanism costs nothing measurable
  on this workload.
* **The overhead is ptrace thread-event handling, amplified by raft-lab's
  design.** This raft implementation spawns short-lived threads per replication
  broadcast (`bcast_heartbeat` creates 3 per broadcast, plus gRPC sync-server
  pool churn) — about **one million thread create/exit events per run**. Every
  one is a debugger stop, and under the MI interpreter every one is also a
  `=thread-created` / `=thread-exited` notification formatted and streamed over
  the ssh channel. That interface cost is real and measurable: an early version
  of this harness used a console/batch gdb, which swallows those notifications
  silently, and measured only −4.6% at 2048 clients — the MI-driven gdb measures
  −12.8% at the same load. Notably, `raft_node` CPU is nearly identical with and
  without a debugger — the cost is added *latency* in the stop-handle-resume
  window of each freshly spawned sender thread, which lands directly on the RPC
  critical path of this serialization-bound pipeline.

So the overhead is `MI gdb ≈ thread-churn ptrace cost + MI event streaming`,
and `ddb ≈ MI gdb` plus a small orchestration margin (DDB consumes the same
event stream remotely). A framework that spawns a thread per message (as this
lab-style raft does) is close to the worst case for any attached debugger; the
Nu socialnet experiment sits at the opposite extreme (stable thread pool, zero
churn — and measured zero attach overhead even at saturation).

One cosmetic artifact worth knowing: the patched gRPC's server-side extraction
prints `[DDB Connector] WARN: Magic doesn't match` for every RPC that arrives
without `bt_meta` (e.g., every proposal from `tput_remote`, which is not
DDB-initialized). It appears in `/tmp/raft_node.log` in **all** modes equally —
~780k lines per run — and is harmless.

## What each mode actually does

**`--mode ddb`** — `start_ddb.sh` launches DDB on node0 with a managed EMQX
broker and writes `tcp://10.10.1.1:<port>` into
`/tmp/ddb/service_discovery/config`, which is copied to all three raft nodes. Each
node, started with `--ddb --ddb_host_ip <its own ip>`, reports itself to the
broker and then **parks in `sigwait()` before it does anything else** — including
before it reports ready to the client. DDB discovers all three, ssh'es in, and
attaches gdb to each. The harness then resumes each session explicitly
(`-exec-continue --session <sid>`), which releases the nodes to join the cluster.

Because the nodes park *before* reporting ready, there is no window in which the
benchmark could start against a not-yet-attached node.

**`--mode gdb`** — each raft_node is launched under a gdb driven through the
**same interface DDB uses**: `--interpreter=mi3`, `-gdb-set mi-async on`, the
same prerun commands, with the MI event stream flowing back over the ssh channel
like DDB's sessions. It does *not* reproduce DDB's broker / `sigwait` / SIG40
handshake — that machinery exists so DDB can discover and attach to
already-running processes; a baseline gdb that launches the node itself doesn't
need it (and being launched under gdb means the node is debugged from birth, so
there is no attach race with the benchmark, and no sudo is needed — raft_node
stays an unprivileged process in every mode).

Do not "simplify" this to a console/batch gdb (`gdb -batch -ex run`): the MI
interpreter emits a notification for every thread create/exit where the console
interpreter stays silent, and this workload generates ~1M thread events per run
— a console-gdb baseline measures a structurally cheaper debugger than the one
inside DDB (we measured it at roughly half the overhead).

**Every run also verifies the debugger *survived* the measurement** (DDB still
up, every node still traced afterwards) and rejects the result otherwise. This
is not hypothetical: DDB once died mid-run when the root filesystem filled, its
orphaned gdbs kept the cluster alive, and the run produced a plausible-looking
but meaningless number.

**Every run verifies its own premise before it measures.** For `ddb`/`gdb` it
checks `TracerPid != 0` on all three nodes (and that none is left stopped); for
`none` it checks `TracerPid == 0`. A mode that silently failed to attach would
otherwise just reproduce the baseline and look like "zero overhead" — the most
dangerous possible failure for this experiment.

## Scripts

| Script | Purpose |
|--------|---------|
| `build_all.sh` | toolchain, gRPC + connector headers, ddb, and a Release build of raft-lab |
| `setup_nodes.sh` | ship the binary + sources to node1–3; install `libpaho-mqtt3c` there |
| `run_benchmark.sh` | **Main entry point.** Brings up the cluster, runs the load, tears down |
| `start_ddb.sh` | DDB + EMQX broker + distribute the service-discovery config |
| `stop_all.sh` | Kill everything, on every node |
| `common.sh` | Internal: topology, paths, remote helpers |

Topology lives in `common.sh` (`HEAD_IP`, `SERVER_IPS`); change those to reshape
the deployment.

## If a step fails

**`raft-lab sources not found`** — see "Getting the raft-lab repo" above; clone
it, or pass `RAFT_DIR=/path/to/it`.

**`cannot talk to docker as <user>`** — your shell predates your `docker` group
membership. Run `newgrp docker` (or log out and back in) and retry.

**`only N/3 nodes attached`** — DDB could not attach to every node. Check
`logs/ddb.log`. Usually passwordless SSH or `sudo` from node0 to that node is
missing, or a stale `raft_node` from an interrupted run is still holding the
port: `./stop_all.sh`.

**`cluster never became ready`** — the raft nodes never all reported in. Per-node
logs are saved to `logs/raft_node.<ip>.log`, and the client's to `logs/client.log`.
The usual cause is a stale process from a previous run; `./stop_all.sh` clears it.

**`raft_node is NOT traced`** — the debugger did not actually attach, so the run
was aborted rather than reporting a meaningless "no overhead" number. Check
`logs/ddb.log` (ddb mode) or `/tmp/raft_node.log` on the node (gdb mode).

**`g++-13 cannot compile <format>`** — the image ships an experimental g++-13
(13.0.0) in `/usr/local/bin` that shadows the real one on `PATH`. `build_all.sh`
installs a real gcc-13 and always calls it by absolute path; it deliberately does
**not** run raft-lab's `scripts/install_gcc-13.sh`, which would repoint the
system-wide `gcc`/`g++` alternatives and break the other experiments on the
machine.
