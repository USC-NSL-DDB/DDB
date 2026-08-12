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

One raft process per machine. 

## One-time setup

```bash
# Toolchain, the DDB-patched gRPC, the DDB connector headers, ddb itself,
# and a Release build of raft_node + tput_remote.
# First run takes ~30 min (gRPC dominates).
./build_all.sh

# Ship the binary + sources to node1..node3.
# Re-run this after every ./build_all.sh.
./setup_nodes.sh
```

## Measure the overhead

Each invocation brings the cluster up, runs the load generator once, and tears
everything down. **One run = one data point.**

```bash
./run_benchmark.sh --mode none    # baseline, no debugger
./run_benchmark.sh --mode ddb     # DDB attached to all 3 raft nodes
./run_benchmark.sh --mode gdb     # bare gdb/MI debugging all 3 raft nodes
```

For repeated trials:

```bash
for i in 1 2 3; do
  for m in none ddb gdb; do ./run_benchmark.sh --mode "$m"; done
done
```

`./stop_all.sh` cleans up by hand if a run is interrupted.

### Choosing the load (`--clients`)

`--clients` defaults to **1024**, this cluster's knee, where debugger overhead is most visible. 

To sweep the load with all three modes:

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

The number to compare between modes is **`avg_tput_kops`** (thousands of ops/sec). Side by side:

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

**`g++-13 cannot compile <format>`** — the OS image ships an experimental g++-13
(13.0.0) in `/usr/local/bin` that shadows the real one on `PATH`. `build_all.sh`
installs a real gcc-13 and always calls it by absolute path; it deliberately does
**not** run raft-lab's `scripts/install_gcc-13.sh`, which would repoint the
system-wide `gcc`/`g++` alternatives and break the other experiments on the
machine.
