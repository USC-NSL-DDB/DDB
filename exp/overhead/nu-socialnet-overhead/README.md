# Nu SocialNetwork — DDB Overhead Experiment (distributed)

Measures the runtime overhead DDB imposes on Nu's `socialNetwork` app running
across four proclet servers. **The overhead is the difference between two runs
at the same offered load** — one with no debugger, one with DDB attached to all
four servers — in achieved throughput (`real_mops`) and tail latency.

## Topology

5 nodes. Caladan binds one Mellanox port via DPDK, so Nu ssh'es over the *other*
10.10.x NIC. Nodes are addressed by the last octet of that ssh network.

| Node | idx | Caladan IP | Role |
|------|-----|-----------|------|
| node0 (this header node) | 1 | ctrl `18.18.1.1` | controller + init_graph + client (+ DDB + EMQX broker) |
| node1 | 2 | `18.18.1.2` | Nu server, `ServiceEntry` proclet, entry 0 |
| node2 | 3 | `18.18.1.3` | Nu server, entry 1 |
| node3 | 4 | `18.18.1.4` | Nu server, entry 2 |
| node4 | 5 | `18.18.1.5` | Nu server, entry 3 — **main (`-m`)**, boots the app |

`kNumEntries = 4`: the main server creates four `ServiceEntry` proclets the
controller spreads one-per-server, and the `DistributedHashTable` shards spread
across all four too, so proclet calls are genuinely cross-node. The client fans
its 200 threads out across the four entry IPs. **Run every command from node0.**

Seeding is native (`build/init_graph` speaks Thrift straight to the backend) —
there is no nginx and no docker storage tier. That is also a hard requirement
here: every node runs iokerneld (Caladan owns its NIC via DPDK), so no node could
host the kernel-mode nginx the HTTP seeder would need.

## Prerequisites

- 5 nodes, passwordless SSH + `sudo` between them
- A Mellanox NIC for Caladan and a second 10.10.x NIC for SSH
- Docker on node0 only (DDB runs its EMQX broker in a container); your user in
  the `docker` group (`newgrp docker`)

## One-time setup

```bash
# Build: connector headers -> ~/.local, caladan + ksched, Nu (CONFIG_DDB=y),
# ddb, and the socialNetwork app (server, client, init_graph).
./build_all.sh

# Prepare all 5 nodes: hugepages + ksched, replicate the Nu tree to the server
# nodes (there is no shared filesystem).
./setup_nodes.sh
```

## Measure the overhead

Each `run_benchmark.sh` invocation brings the whole cluster up, seeds the graph,
runs the client once, and tears everything down. One run = one data point.

```bash
# Baseline and DDB-attached at the SAME load. --mops is the total target
# throughput (Mops); the client uses 200 threads and defaults to --mops 1.0.
./run_benchmark.sh              --mops 1.0     # baseline (no debugger)
./run_benchmark.sh --ddb        --mops 1.0     # DDB attached to all 4 servers
```

A single pair is enough to sanity-check the pipeline, but runs vary ~1% between
trials, so for a real number **take several trials of each and compare the
medians at matched load**:

```bash
for i in 1 2 3 4 5; do ./run_benchmark.sh       --mops 1.0; done
for i in 1 2 3 4 5; do ./run_benchmark.sh --ddb --mops 1.0; done
```

To see how the overhead behaves with load, repeat the pair across a sweep (push
`--mops` up until `real_mops` stops tracking the target — that is the saturation
point, and the most interesting region for overhead):

```bash
for m in 0.5 1.0 1.5 2.0 2.5 3.0; do
  ./run_benchmark.sh       --mops "$m"
  ./run_benchmark.sh --ddb --mops "$m"
done
```

`./stop_all.sh` cleans up by hand if a run is interrupted (kills every node's
processes, frees stale Caladan shm / hugepages).

## Read the results

Each run writes `results/mops<M>_n4_{baseline,ddb}_<timestamp>.txt`, ending in
the client's summary line:

```
real_mops, avg_lat, 50th_lat, 90th_lat, 95th_lat, 99th_lat, 99.9th_lat
0.969433 196 104 489 766 1266 1851
```

`real_mops` is achieved throughput (millions of ops/sec); the rest are latencies
in microseconds. Overhead at a given load = the baseline-vs-DDB difference in
these numbers. A quick side-by-side:

```bash
for f in results/mops1.0_n4_*.txt; do
  printf '%-40s %s\n' "$(basename "$f")" "$(tail -1 "$f")"
done
```

Reference numbers from a 5-node c6525 cluster at 1.0 Mops (single trials):

| | real_mops | p50 | p99 | p99.9 |
|---|---|---|---|---|
| baseline | 0.968 | 105µs | 1279µs | 1883µs |
| DDB attached (all 4 servers) | 0.960 – 0.969 | 104–106µs | 1266–1302µs | 1851–1935µs |

At steady state with no breakpoints set, an attached gdb costs essentially
nothing — the interesting overhead shows up under breakpoint/stepping workloads
and near saturation, not at idle attach.

## What `--ddb` does

`start_ddb.sh` launches DDB on node0 with a managed EMQX broker and writes
`tcp://<node0>:10101` into `/tmp/ddb/service_discovery/config`, copied to every
server node. Each server, launched with `--ddb --ddb_node_ip <its ip>`, reports
itself to the broker and parks in `sigwait`; DDB discovers all four, ssh'es in,
and attaches gdb to each.

**Attaching freezes each server, and resume order matters.** The harness:

1. starts the three **plain** servers, then resumes them one at a time
   (`-exec-continue --session <sid>`, a pause between each) so each finishes its
   Nu runtime init and registers with the controller;
2. only then starts the **main** (`-m`) server and resumes it last.

The main server's `DoWork` creates and places proclets across the cluster, so it
must run against an already-registered set of servers. Resuming all four in
lockstep (a broadcast `-exec-continue`) makes them race distributed startup and
reliably segfaults one with a NULL `get_runtime()` in the RPC archive pool. This
phased order avoids it and mirrors the original `nu_multi`.

The run **verifies DDB is attached to every server before it measures** and
aborts otherwise. To check by hand:

```bash
for ip in 10.10.2.2 10.10.2.3 10.10.2.4 10.10.2.5; do
  ssh "$ip" 'p=$(pgrep -x main); sudo grep -E "TracerPid|^State" /proc/$p/status'
done
# TracerPid: <gdb pid> + State: S on all four = attached and running.
```

## Scripts

| Script | Purpose |
|--------|---------|
| `build_all.sh` | connector headers, caladan + ksched, Nu (CONFIG_DDB=y), ddb, socialNetwork |
| `setup_nodes.sh` | hugepages/ksched + replicate the Nu tree to the server nodes |
| `run_benchmark.sh` | **Main entry point.** Brings up the cluster, seeds, runs the client |
| `start_ddb.sh` | DDB + EMQX broker + distribute the service-discovery config |
| `stop_all.sh` | Kill everything; clear stale Caladan shm / hugepages |
| `common.sh` | Internal: node roles, NIC detection, remote helpers |

Server count is not a flag — it is the four indices in `SERVER_IDXS` in
`common.sh`. Change that array (and make sure the caladan IPs stay `18.18.1.2+`)
to run a different number of servers.

## If a step fails

Every script stops at the failure and prints what to do. Failure modes worth
naming:

**`Shared memory region is already mapped`** — a SIGKILLed iokerneld left SysV
shm + `/dev/hugepages/rtemap_*` behind. `./stop_all.sh` clears both on every node.

**`backend never came up` with one server DEAD under `--ddb`** — the resume race
above; the harness saves each server's full log to `logs/backend.idx*.log`. Keep
the phased, per-session resume; do not resume servers in lockstep.

**Client dies in `TSocket::openConnection`** — the client's `kNumEntries`
disagrees with the servers'. `run_benchmark.sh` pins both to the server count on
every run, so this only bites if you edit the sources by hand.
