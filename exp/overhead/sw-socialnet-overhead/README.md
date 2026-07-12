# ServiceWeaver SocialNet Overhead Experiment

Measures throughput and tail latency of the ServiceWeaver SocialNetwork app on a
k3s cluster, with and without DDB attached.

## Topology

- 5 nodes, listed in `cluster.txt`: `10.10.1.1` – `10.10.1.5`
- **node0** (`10.10.1.1`): k3s master + load generator. Tainted `NoSchedule`, so no app pods land here.
- **node1–node4**: k3s workers running all 14 app pods.

Run every command below **on node0**, from this directory.

## Run the benchmark

```bash
# 1. Install k3s, Go, Docker, python-kubernetes on all nodes; weaver-kube +
#    submodules on node0. Verifies every node and fails if one is incomplete.
./deploy_all.sh

# 2. Pick up docker group membership (usermod does not affect the current shell).
newgrp docker

# 3. Start k3s, join workers as node1..node4, build the app, deploy it to
#    Kubernetes, expose it as a NodePort, tune TCP. Idempotent.
./setup_experiment.sh

# 4. Seed the social graph (962 users, 18K follows from socfb-Reed98).
#    Required after every redeploy or rescale -- state is in-memory.
./seed_data.sh

# 5. Sanity check: expect 5 nodes on 10.10.1.x and "Health: HTTP 404".
./check_cluster.sh

# 6. Measure. Results land in results/.
./run_benchmark.sh
```

## Measure DDB overhead

```bash
# 1. Deploy the ssh-gateway, inject debug sidecars into all 14 pods, and render
#    a ready-to-use DDB config. Re-run after any redeploy or rescale: pod
#    restarts drop the ephemeral sidecars.
./setup_ddb.sh

# 2. Attach DDB.
ddb ddb/serviceweaver_config.yaml
```

**Attaching freezes the app.** DDB stops every process on attach, so the endpoint
times out until you resume it. 
Use `-exec-continue` to continue the execution.

```
(ddb) -exec-continue
```

Then, in a second shell, run `./run_benchmark.sh` with the same flags as your
baseline and compare. Type `exit` in the REPL to detach.

### Confirm DDB is really attached

Running sidecars do **not** mean the debugger is attached. Ask the kernel:

```bash
POD=$(kubectl get pods --no-headers -o name | grep weaver-main | cut -d/ -f2)
kubectl exec "$POD" -c serviceweaver -- \
  sh -c 'p=$(pgrep -f "^/weaver/server.out"); grep -E "TracerPid|^State" /proc/$p/status'
```

- `TracerPid: 0` → nothing attached.
- `TracerPid: <pid>` + `State: t (tracing stop)` → attached, app **frozen**; send `-exec-continue`.
- `TracerPid: <pid>` + `State: S (sleeping)` → attached and running. Benchmark now.

## Between experiments

Pod restarts clear in-memory state and drop the debug sidecars.

```bash
./redeploy_app.sh              # fresh pods (--full: teardown + re-apply manifests)
./scale_cluster.sh set 2       # or: vary worker count for cluster-size sweeps
./seed_data.sh                 # always required afterwards
./setup_ddb.sh                 # only if using DDB
```

## Benchmark parameters

| Flag | Default | Description |
|------|---------|-------------|
| `--target-mops` | `0.00005` | Target MOPS per thread (Poisson arrivals) |
| `--threads` | `10` | Concurrent benchmark threads |
| `--duration` | `120` | Measurement seconds |
| `--warmup` | `4` | Warmup seconds |
| `--addr` | auto | Override the API endpoint |
| `--sweep` | — | Comma-separated MOPS values to sweep |

Injection rate = `target-mops × threads`. 
Workload: 60% read user timeline, 30% read home timeline, 5% compose post, 5% remove posts.

## Output

```
results/mops0.00005_t10_d120_<ts>.txt             # throughput + latency
results/mops0.00005_t10_d120_<ts>_timeseries.txt  # p99 over time
```

```
real_mops, avg_lat, 50th_lat, 90th_lat, 95th_lat, 99th_lat, 99.9th_lat
0.004993 812 744 1077 1406 1891 2305
```

`real_mops` is achieved throughput (millions of ops/sec); latencies are in
microseconds. Check the `requests: N generated, N served, 0 skipped` line — a
nonzero skip count means the client could not keep up with its schedule.

## Scripts

| Script | Purpose |
|--------|---------|
| `deploy_all.sh` | Install dependencies on every node |
| `setup_experiment.sh` | **Main entry point:** k3s, join, build, deploy, expose, tune |
| `seed_data.sh` | Load the social graph |
| `run_benchmark.sh` | Run the load client |
| `setup_ddb.sh` | ssh-gateway + debug sidecars + rendered DDB config |
| `check_cluster.sh` | Health check: nodes, pods, endpoint, sidecars |
| `redeploy_app.sh` | Restart pods to clear state |
| `scale_cluster.sh` | Add/remove workers |
| `build_app.sh`, `deploy_app.sh` | Called by `setup_experiment.sh` |
| `join_cluster.sh`, `common.sh` | Internal |

## If a step fails

Each script stops at the failure and prints the exact remedy (missing docker
group, a worker that never joined, k3s not coming up). Follow that message and
re-run the script — all of them are idempotent.
