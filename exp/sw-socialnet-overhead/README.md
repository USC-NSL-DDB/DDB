# ServiceWeaver SocialNet Overhead Experiment

Measures throughput and tail latency of the ServiceWeaver SocialNetwork application deployed on a k3s cluster.

## Cluster Topology

- **5 nodes** (defined in `cluster.txt`): `10.10.1.1` – `10.10.1.5`
- **node0** (`10.10.1.1`): k3s master + load generator (no app pods)
- **node1–node4**: k3s workers running all app microservice pods (14 services, 1 replica each)
- The benchmark client runs on node0 to avoid competing with app pods for CPU.

## Prerequisites

- All nodes provisioned via `deploy_all.sh` (installs Docker, Go, k3s, etc.)
- k3s server already running on node0
- SSH access from node0 to all workers without password

## Quick Start

```bash
# 1. One-time setup: join workers, expose service, build binaries
./setup_experiment.sh

# 2. Seed the social graph (962 users, 18K follows from socfb-Reed98 dataset)
./seed_data.sh

# 3. Run benchmark with default settings
./run_benchmark.sh

# 4. Run at higher load
./run_benchmark.sh --target-mops 0.0001

# 5. Sweep multiple load levels
./run_benchmark.sh --sweep 0.00005,0.0001,0.0005,0.001

# 6. (Optional) Redeploy app with fresh state before a new experiment
./redeploy_app.sh
./seed_data.sh
./run_benchmark.sh

# 7. (Optional) Set up DDB debugger sidecars before attaching DDB
./setup_ddb.sh
```

# 8. (Optional) Scale to fewer workers for cluster size experiments
./scale_cluster.sh set 2      # run with 2 workers
./seed_data.sh
./run_benchmark.sh
./scale_cluster.sh set 4      # restore to 4 workers

## Scripts

| Script | Purpose |
|--------|---------|
| `setup_experiment.sh` | Join workers to k3s, patch service to NodePort, tune TCP, build binaries |
| `seed_data.sh` | Seed social graph data (users, follows, posts) into the running app |
| `run_benchmark.sh` | Run throughput benchmark with configurable parameters |
| `redeploy_app.sh` | Redeploy app with fresh pods (clears in-memory state) |
| `check_cluster.sh` | Health check: node reachability, pod distribution, endpoint status |
| `join_cluster.sh` | (Internal) Join worker nodes to k3s cluster |
| `setup_ddb.sh` | Inject debug sidecar containers for DDB (must run before attaching debugger) |
| `scale_cluster.sh` | Scale worker nodes up/down for cluster size experiments |

## Benchmark Parameters

| Flag | Default | Description |
|------|---------|-------------|
| `--target-mops` | `0.00005` | Target MOPS per thread (Poisson arrival rate) |
| `--threads` | `10` | Number of concurrent benchmark threads |
| `--duration` | `120` | Measurement duration in seconds |
| `--warmup` | `4` | Warmup duration in seconds |
| `--addr` | auto-detected | Override API endpoint URL |
| `--sweep` | — | Comma-separated MOPS values to sweep |

**Effective injection rate** = `target-mops × threads`. For example, `--target-mops 0.0001 --threads 10` injects at 0.001 MOPS (1K ops/sec) total.

## Workload Mix

| Operation | Proportion |
|-----------|------------|
| Read user timeline | 60% |
| Read home timeline | 30% |
| Compose post | 5% |
| Remove posts | 5% |

## Output

Results are saved to `results/` with timestamps:
```
results/mops0.00005_t10_d120_20260301_170000.txt        # throughput + latency summary
results/mops0.00005_t10_d120_20260301_170000_timeseries.txt  # p99 latency timeseries
```

Each result file contains:
```
real_mops, avg_lat, 50th_lat, 90th_lat, 95th_lat, 99th_lat, 99.9th_lat
0.004999 1287 1161 1809 2430 3929 4901
```
- `real_mops`: Achieved throughput in millions of ops/sec
- `*_lat`: Latency in microseconds

## Network Setup Details

The app is exposed via k8s **NodePort** (not LoadBalancer or port-forward) to avoid introducing proxy bottlenecks. Traffic path:

```
client.out (node0)
  → 10.10.1.1:<NodePort>  (kube-proxy)
  → weaver-main pod       (HTTP entry point)
  → microservice pods     (inter-pod gRPC, across nodes 1-4)
```

TCP kernel tuning (applied by `setup_experiment.sh`) prevents ephemeral port exhaustion under high connection rates:
- `tcp_tw_reuse=1` — reuse TIME_WAIT sockets
- `ip_local_port_range=1024-65535` — maximize available ephemeral ports
- `tcp_fin_timeout=15` — faster socket recycling

## Redeploying the App

By default, `run_benchmark.sh` only runs the load client — it does not redeploy the app. If you need fresh state between experiments (e.g., to clear caches or test cold-start):

```bash
# Rolling restart (default): new pods, same config, ~30s
./redeploy_app.sh

# Full teardown + re-apply from saved manifests
./redeploy_app.sh --full
```

After either mode, re-seed the social graph before benchmarking:
```bash
./seed_data.sh
./run_benchmark.sh
```

The saved manifests (`socialnet-manifests.yaml`) are exported during setup. To re-export:
```bash
kubectl get deployments,services,hpa -o yaml -n default > socialnet-manifests.yaml
```

## DDB (Distributed Debugger) Setup

To attach DDB to the running cluster, you must first inject debug sidecar containers into every app pod. These ephemeral containers provide the SSH daemon that DDB needs to connect through the ssh-gateway bastion.

```bash
# Inject debug sidecars into all app pods (run on master node only)
./setup_ddb.sh

# Check sidecar status without injecting
./setup_ddb.sh --check
```

**Important notes:**
- Run `setup_ddb.sh` only on the **master node** (node0) — it uses the k8s API to inject sidecars across all worker nodes
- Must be run **after** the app is deployed and pods are running
- Must be re-run after every `redeploy_app.sh` since redeployment creates new pods without sidecars
- The script is idempotent — re-running when sidecars are already present is a no-op

Once sidecars are running, launch DDB with:
```bash
ddb --config /local/tmp/serviceweaver_config.yaml
```

DDB connection path:
```
DDB → ssh-gateway bastion (10.43.226.232:2222) → debug sidecar (port 22) → app process
```

## Scaling Experiments (Varying Worker Count)

To measure performance under different cluster sizes, use `scale_cluster.sh` to add/remove worker nodes:

```bash
# Check current state
./scale_cluster.sh status

# Scale to N workers (removes highest-numbered nodes first)
./scale_cluster.sh set 2
./seed_data.sh
./run_benchmark.sh

# Scale to a different size
./scale_cluster.sh set 1
./seed_data.sh
./run_benchmark.sh

# Restore all 4 workers
./scale_cluster.sh set 4
./seed_data.sh
./run_benchmark.sh

# Or add/remove specific nodes
./scale_cluster.sh remove node3 node4
./scale_cluster.sh add node4
./scale_cluster.sh add all
```

**How it works:**
- **Removing** workers: cordons the node (prevents new scheduling) then drains it (evicts pods). Pods are rescheduled onto remaining active workers.
- **Adding** workers: uncordons the node then triggers a rolling restart so pods redistribute across all active workers.
- Node0 (master/load-generator) is never touched.

**After every scaling operation**, re-seed data and optionally re-inject DDB sidecars:
```bash
./seed_data.sh                 # required — pods restarted, in-memory state lost
./setup_ddb.sh                 # only if using DDB
```
