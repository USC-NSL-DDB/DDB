# Nu SocialNetwork — DDB Overhead Experiment (distributed)

Measures the runtime overhead DDB imposes on Nu's `socialNetwork` app running
across four proclet servers. **The overhead is the difference between two runs
at the same offered load** — one with no debugger, one with DDB attached to all
four servers — in achieved throughput (`real_mops`) and tail latency.

## Topology

5 nodes. Caladan binds one Mellanox port via DPDK, so Nu ssh'es over the *other*
10.10.x NIC. Nodes are addressed by the last octet of that ssh network.

| Node | idx | ssh IP | Role |
|------|-----|--------|------|
| node0 | 1 | `10.10.2.1` | controller (`18.18.1.1`) + DDB + EMQX broker + init_graph + **all 3 clients** |
| node1 | 2 | `10.10.2.2` | Nu server, entry 0, caladan `18.18.1.2` |
| node2 | 3 | `10.10.2.3` | Nu server, entry 1, caladan `18.18.1.3` |
| node3 | 4 | `10.10.2.4` | Nu server, entry 2, caladan `18.18.1.4` |
| node4 | 5 | `10.10.2.5` | Nu server, entry 3, caladan `18.18.1.5` — **main (`-m`)**, boots the app |

`kNumEntries = 4`: the main server creates four `ServiceEntry` proclets the
controller spreads one-per-server, and the `DistributedHashTable` shards spread
across all four too, so proclet calls are genuinely cross-node. **Run every
command from node0.**

Seeding is native (`build/init_graph` speaks Thrift straight to the backend) —
no nginx, no docker storage tier. That is also a hard requirement here: every
node runs iokerneld (Caladan owns its NIC via DPDK), so no node could host the
kernel-mode nginx the HTTP seeder would need.

### How load is generated (Nu's `nu_multi` mechanism)

The client is Nu's distributed benchmark client (`nu_multi/client.cpp`):
**three** client processes, each 200 threads, driven by `run_multi_clients` with
a TCP barrier so they start together. Each offers `--mops / 3`, and **total
throughput is the sum of the three per-client `real_mops`**. All three are
co-located on node0 — the server side saturates first (see below), so a single
client node is sufficient. (Real Nu spreads clients across separate machines;
co-locating is a deviation that's harmless only because the servers bind first.)

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

# Prepare all nodes: hugepages + ksched, replicate the Nu tree to every
# non-local node (there is no shared filesystem).
./setup_nodes.sh
```

## Measure the overhead

Each `run_benchmark.sh` invocation brings the whole cluster up, seeds the graph,
runs the clients once, and tears everything down. One run = one data point.

```bash
# Baseline and DDB-attached at the SAME load. --mops is the TOTAL target
# throughput (Mops), split across the 3 clients; it defaults to 1.0.
./run_benchmark.sh              --mops 1.0     # baseline (no debugger)
./run_benchmark.sh --ddb        --mops 1.0     # DDB attached to all 4 servers
```

A single pair sanity-checks the pipeline, but runs vary ~1% between trials, so
for a real number take several trials of each and compare the medians at matched
load:

```bash
for i in 1 2 3 4 5; do ./run_benchmark.sh       --mops 1.0; done
for i in 1 2 3 4 5; do ./run_benchmark.sh --ddb --mops 1.0; done
```

To see how overhead behaves with load, repeat the pair across a sweep. Note the
servers saturate near ~1 Mops (below), so pushing `--mops` far past that just
raises latency:

```bash
for m in 0.5 1.0 1.5 2.0; do
  ./run_benchmark.sh       --mops "$m"
  ./run_benchmark.sh --ddb --mops "$m"
done
```

`./stop_all.sh` cleans up by hand if a run is interrupted (kills every node's
processes, frees stale Caladan shm / hugepages).

## Read the results

Each run writes `results/mops<M>_n4_{baseline,ddb}_<timestamp>.txt`. With the
multi-client mechanism it holds one line per client plus the aggregate:

```
# per-client (real_mops avg 50th 90th 95th 99th 99.9th):
client1 0.313926 636 399 1475 2043 3388 6221
client2 0.313939 636 398 1478 2044 3388 6208
client3 0.320823 622 390 1457 2022 3363 6003
aggregate_real_mops 0.948688
```

The number to compare between baseline and `--ddb` is **`aggregate_real_mops`**
(millions of ops/sec, summed across the 3 clients); per-client latencies are in
microseconds. A quick side-by-side of a load point:

```bash
for f in results/mops1.0_n4_*.txt; do
  printf '%-40s %s\n' "$(basename "$f")" "$(grep aggregate_real_mops "$f")"
done
```

Reference numbers from this cluster (4 servers, single trials):

| | aggregate real_mops | notes |
|---|---|---|
| baseline | ~0.95 | server-side saturation |
| DDB attached (all 4 servers) | ~0.95 | within run-to-run noise of baseline |

The ceiling is **server-side**: throughput scales with server count
(~0.24 Mops/server — 3 servers give ~0.71, 4 give ~0.95) and does not rise with
more client threads, CPU, or client nodes. At steady state with no breakpoints,
an attached gdb costs essentially nothing; the interesting overhead shows up
under breakpoint/stepping workloads, not at idle attach.

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
| `setup_nodes.sh` | hugepages/ksched + replicate the Nu tree to every non-local node |
| `run_benchmark.sh` | **Main entry point.** Brings up the cluster, seeds, runs the clients |
| `start_ddb.sh` | DDB + EMQX broker + distribute the service-discovery config |
| `stop_all.sh` | Kill everything; clear stale Caladan shm / hugepages |
| `common.sh` | Internal: node roles, NIC detection, remote helpers |

Node roles live in `common.sh`: `SERVER_IDXS` is the server node indices (their
caladan IPs must stay `18.18.1.2+`), and `CLIENT_NODES` lists which node hosts
each of the 3 clients. Change those arrays to re-shape the deployment.

## If a step fails

Every script stops at the failure and prints what to do. Failure modes worth
naming:

**`Shared memory region is already mapped`** — a SIGKILLed iokerneld left SysV
shm + `/dev/hugepages/rtemap_*` behind. `./stop_all.sh` clears both on every node.

**`iokerneld failed on <node>`** — Caladan's `ias` scheduler could not start on
that node (e.g. a host whose BIOS exposes many NUMA domains). Use a node with a
flat NUMA topology, or drop it from the role arrays in `common.sh`.

**`backend never came up` with one server DEAD under `--ddb`** — the resume race
above; the harness saves each server's full log to `logs/backend.idx*.log`. Keep
the phased, per-session resume; do not resume servers in lockstep.

**Client dies in `TSocket::openConnection`** — the client's `kNumEntries`
disagrees with the servers'. `run_benchmark.sh` pins both to the server count on
every run, so this only bites if you edit the sources by hand.
