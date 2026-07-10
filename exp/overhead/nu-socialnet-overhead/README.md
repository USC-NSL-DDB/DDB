# Nu SocialNetwork Overhead Experiment

Measures throughput and tail latency of Nu's `socialNetwork` app, with and
without DDB attached to the Nu backend.

## Topology

Nodes are addressed by Nu's `exp/shared.sh` index (`ssh_ip N`). Caladan binds one
Mellanox port via DPDK, so Nu SSHes over the **other** `10.10.x` network.

| Index | Role | Runs iokerneld |
|-------|------|----------------|
| node1 | Nu backend (`build/src/main`), DDB, EMQX broker | yes |
| node2 | Nu controller (`bin/ctrl_main`) | yes |
| node3 | nginx + the DeathStarBench storage stack (docker) | **no** — DPDK would seize its NIC |
| node4 | benchmark client (`build/bench/client`) | yes |

The backend must stay at index 1: nginx's config hard-codes its caladan IP
(`18.18.1.2`). Run every command below from **node1**, in this directory.

## Prerequisites

- 4 nodes, passwordless SSH + `sudo` between them
- A Mellanox NIC for Caladan and a second `10.10.x` NIC for SSH
- Docker on node1 (DDB runs its EMQX broker in a container) and node3

## Run the benchmark

```bash
# 1. Build everything: connector headers -> ~/.local/include, caladan + ksched,
#    Nu with CONFIG_DDB=y, ddb itself, then the socialNetwork app.
./build_all.sh

# 2. Prepare all four nodes: hugepages + ksched, replicate the repo to the same
#    absolute path (there is no shared FS), docker + aiohttp on the nginx node.
./setup_nodes.sh

# 3. Baseline: no debugger.
./run_benchmark.sh --mops 0.3

# 4. With DDB attached to the backend. Needs docker without sudo (`newgrp docker`).
./run_benchmark.sh --ddb --mops 0.3
```

Each run tears down the previous one first. `./stop_all.sh` cleans up by hand;
`./stop_all.sh --full` also takes the storage stack down.

## What `--ddb` does

`start_ddb.sh` launches DDB, which starts a managed EMQX broker and writes
`tcp://<node1>:10101` into `/tmp/ddb/service_discovery/config`. That file is
copied to the other Caladan nodes, because every Nu process launched with
`--ddb` reads it to find the broker.

The backend is then started with:

```
--ddb --ddb_node_ip <node1-ssh-ip> --ddb_sd_config_path /tmp/ddb/service_discovery/config
```

It reports itself to the broker and parks in `sigwait`. DDB discovers it, SSHes
in, attaches gdb, and sends `signal SIG40`; the connector's handler then raises
`SIGTRAP` so the process stops for inspection. **The app is frozen at this
point.** `run_benchmark.sh` waits for that trap and resumes it with
`-exec-continue` before seeding and measuring.

To drive a session by hand instead:

```bash
./start_ddb.sh                              # leaves DDB running
echo '-exec-continue' > logs/ddb_in         # the REPL takes GDB/MI commands
```

### Confirming DDB is really attached

Session logs are not proof. `run_benchmark.sh --ddb` asks the kernel before it
measures, and fails the run if the answer is wrong:

```
TracerPid=191196 State=S  ->  attached and running
```

- `TracerPid: 0` → nothing attached.
- `TracerPid: <pid>` + `State: t` → attached but **frozen**; it never resumed.
- `TracerPid: <pid>` + `State: S` → attached and running. Numbers are valid.

## Output

`results/mops<M>_{baseline,ddb}_<timestamp>.txt`, ending in the client's summary:

```
real_mops, avg_lat, 50th_lat, 90th_lat, 95th_lat, 99th_lat, 99.9th_lat
0.299808 45 27 89 171 300 481
```

`real_mops` is achieved throughput (millions of ops/sec); latencies are in
microseconds. Compare a `--ddb` run against a baseline run at the **same**
`--mops`.

Measured on a 4-node c6525 cluster at 0.3 Mops:

| | real_mops | p50 | p99 | p99.9 |
|---|---|---|---|---|
| baseline | 0.299698 | 27µs | 297µs | 479µs |
| DDB attached | 0.299808 | 27µs | 300µs | 481µs |

An attached-but-running gdb with no breakpoints costs essentially nothing at
steady state.

## Scripts

| Script | Purpose |
|--------|---------|
| `build_all.sh` | connector headers, caladan + ksched, Nu (CONFIG_DDB=y), ddb, socialNetwork |
| `setup_nodes.sh` | hugepages/ksched, repo replication, docker + aiohttp on nginx node |
| `run_benchmark.sh` | **Main entry point.** Brings the cluster up, seeds, measures |
| `start_ddb.sh` | DDB + EMQX broker + distributes the service-discovery config |
| `stop_all.sh` | Kill everything; clear stale SysV shm and DPDK hugepages |
| `common.sh` | Internal: node roles, NIC detection, remote helpers |

## If a step fails

Each script stops at the failure and prints what to do. Two failure modes are
worth naming because their symptoms are opaque:

**`Shared memory region is already mapped`** — a SIGKILLed `iokerneld`, or a Nu
process still attached to its segments, leaves SysV shm and
`/dev/hugepages/rtemap_*` behind. `./stop_all.sh` clears both.

**Client dies in `TSocket::openConnection` with `ASSERTION '!socket_'`** — the
client's `kNumEntries` disagrees with the backend's, so it dials a node that
does not exist. `run_benchmark.sh` pins both to 1 on every run; the checked-in
`bench/client.cpp` can be left at 7 by the `nu_multi` experiment.
