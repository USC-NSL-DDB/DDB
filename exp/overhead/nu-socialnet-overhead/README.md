# Nu SocialNetwork — DDB Overhead Experiment

Measures the runtime overhead DDB imposes on Nu's `socialNetwork` app running
across four proclet servers. **The overhead is the difference between two runs
at the same offered load** — one with no debugger, one with DDB attached to all
four servers — in achieved throughput (`real_mops`) and tail latency.

## Topology

5 nodes. Caladan binds one Mellanox port via DPDK, so Nu ssh'es over the *other*
10.10.x NIC. 

| Node | idx | ssh IP | Role |
|------|-----|--------|------|
| node0 | 1 | `10.10.2.1` | controller (`18.18.1.1`) + DDB + EMQX broker + init_graph + **all 3 clients** |
| node1 | 2 | `10.10.2.2` | Nu server, entry 0, caladan `18.18.1.2` |
| node2 | 3 | `10.10.2.3` | Nu server, entry 1, caladan `18.18.1.3` |
| node3 | 4 | `10.10.2.4` | Nu server, entry 2, caladan `18.18.1.4` |
| node4 | 5 | `10.10.2.5` | Nu server, entry 3, caladan `18.18.1.5` — **main (`-m`)**, boots the app |

`kNumEntries = 4`: the main server creates four `ServiceEntry` proclets the
controller spreads one-per-server, and the `DistributedHashTable` shards spread
across all four too, so proclet calls are genuinely cross-node. 

**Run everycommand from node0.**

## Setup

```bash
# Build: connector headers -> ~/.local, caladan + ksched, Nu (CONFIG_DDB=y),
# ddb, and the socialNetwork app (server, client, init_graph).
./build_all.sh

# Prepare all nodes: hugepages + ksched, replicate the Nu tree to every node.
./setup_nodes.sh
```

## Measure the overhead

Each `run_benchmark.sh` invocation brings the whole cluster up, seeds the graph,
runs the clients once, and tears everything down.

```bash
# All three cases at the SAME load. --mops is the TOTAL target throughput
# (Mops), split across the 3 clients; it defaults to 1.0.
./run_benchmark.sh --vanilla    --mops 1.0     # vanilla Nu (CONFIG_DDB=n): no
                                               #   DDB metadata embedding at all
./run_benchmark.sh              --mops 1.0     # baseline: instrumented build
                                               #   (CONFIG_DDB=y), no debugger
./run_benchmark.sh --ddb        --mops 1.0     # DDB attached to all 4 servers
```

The three pairwise comparisons mean different things: **vanilla vs baseline** is
the cost of Nu's compiled-in DDB instrumentation (paid whether or not anyone
debugs), **baseline vs ddb** is the cost of actually attaching, and **vanilla vs
ddb** is DDB's total cost on Nu.

You can also do repeated trials of each and compare the medians at matched load:

```bash
for i in 1 2 3 4 5; do ./run_benchmark.sh --vanilla --mops 1.0; done
for i in 1 2 3 4 5; do ./run_benchmark.sh           --mops 1.0; done
for i in 1 2 3 4 5; do ./run_benchmark.sh --ddb     --mops 1.0; done
```

`./stop_all.sh` cleans up by hand if a run is interrupted (kills every node's
processes, frees stale Caladan shm / hugepages).

## Read the results

Each run writes `results/mops<M>_n4_{vanilla,baseline,ddb}_<timestamp>.txt`.
With the multi-client mechanism it holds one line per client plus the
aggregate:

```
# per-client (real_mops avg 50th 90th 95th 99th 99.9th):
client1 0.313926 636 399 1475 2043 3388 6221
client2 0.313939 636 398 1478 2044 3388 6208
client3 0.320823 622 390 1457 2022 3363 6003
aggregate_real_mops 0.948688
```

The number to compare across modes is **`aggregate_real_mops`**
(millions of ops/sec, summed across the 3 clients); per-client latencies are in
microseconds. A quick side-by-side of a load point:

```bash
for f in results/mops1.0_n4_*.txt; do
  printf '%-40s %s\n' "$(basename "$f")" "$(grep aggregate_real_mops "$f")"
done
```

Reference numbers from this cluster (4 servers, `--mops 1.0`, medians of 3
trials, 2026-07-11):

| | aggregate real_mops | client p50 (µs) | vs vanilla |
|---|---|---|---|
| vanilla Nu (`CONFIG_DDB=n`) | 0.999 | 237 | — |
| baseline (`CONFIG_DDB=y`, no debugger) | 0.968 | 368 | **−3.2% tput, +55% p50** |
| DDB attached (all 4 servers) | 0.965 | 369 | **−3.4% tput** |

## Scripts

| Script | Purpose |
|--------|---------|
| `build_all.sh` | connector headers, caladan + ksched, Nu (CONFIG_DDB=y), ddb, socialNetwork |
| `setup_nodes.sh` | hugepages/ksched + replicate the Nu tree to every non-local node |
| `run_benchmark.sh` | **Main entry point.** Brings up the cluster, seeds, runs the clients |
| `start_ddb.sh` | DDB + EMQX broker + distribute the service-discovery config |
| `stop_all.sh` | Kill everything; clear stale Caladan shm / hugepages |
| `diag_backend.sh` | Re-run one backend with a verbose caladan conf (+`--strace`) to expose init errors |
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

**Server log ends in a bare `failed to start runtime`** — Nu's auto-generated
caladan conf hard-codes `log_level 0`, which silences every real error on the
runtime-init path. `run_benchmark.sh` now re-runs one backend with a verbose
conf automatically when this happens (see `logs/backend.diag.log`); you can
also do it by hand with `./diag_backend.sh <idx> --strace`. The classic cause
is `Could not initialize directpath, ret = -12`: the backend silently resolved
the **system** libmlx5/libibverbs instead of caladan's patched
`rdma-core/build/lib` (check with `ldd build/src/main` on the server node —
`run_benchmark.sh` also asserts this on every run). That happens when the
binary's RUNPATH anchor dir (`single_proclet/src`) is missing on that node;
re-running `./setup_nodes.sh` ships it.

**Client dies in `TSocket::openConnection`** — the client's `kNumEntries`
disagrees with the servers'. `run_benchmark.sh` pins both to the server count on
every run, so this only breaks if you edit the sources by hand.
