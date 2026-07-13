# ServiceWeaver SocialNet Call-Depth Experiment

Measure DDB backtrace latency at RPC depths 1, 2, and 3.

## Requirements

- One physical host.
- Linux with systemd and passwordless or interactive `sudo` access.
- `python3`, `curl`, `patch`, Docker, Cargo, Go, and Git.
- Registry access for the build, debugger, and gateway images
- SocialNet commit `613f316ca060b94545e850324f91eef1ceb7639b`

`setup` checks and, when missing, installs native k3s `v1.36.2+k3s1`,
`kubectl` `v1.36.2`, and `weaver-kube` `v0.23.0`. The k3s installation uses
the official installer and may prompt for the sudo password. Existing active
native-k3s installations are reused.

Run every command on the experiment host. The recipe rejects multi-node
clusters. All experiment configuration and test code is in this directory;
only DDB Rust and ServiceWeaver SocialNet are external source inputs.

## 1. Configure

No configuration file is needed on a new machine. Run `setup` first; it
installs the missing Kubernetes tools and detects the local paths. If setup
reports an incorrect path or an existing nonstandard k3s installation, create
the local override file:

```bash
cp artifact.env.example artifact.env
```

Set only the applicable host-specific values:

```bash
TARGET_NODE=<kubernetes-node-name>
KUBECONFIG=/path/to/native-k3s.yaml
K3S_SERVICE=<active-k3s-systemd-unit>

DDB_REPO_ROOT=/absolute/path/to/DDB
DDB_SOURCE_DIR=/absolute/path/to/DDB/ddb
SOCIALNET_DIR=/absolute/path/to/DDB/fwks/socialnetwork
DDB_BIN=/absolute/path/to/DDB/ddb/target/release/ddb
```

On a new machine, leave `TARGET_NODE`, `KUBECONFIG`, and `K3S_SERVICE` unset;
setup fills them from the k3s installation. Set them only when reusing a
nonstandard existing native-k3s service. The recipe automatically discovers
the SocialNet NodePort, so no endpoint setting is needed for a normal run.

The topology is built into the recipe, not configured by the evaluator.
Before measuring, `setup` and `check` require exactly one Ready k3s node and
14 Ready SocialNet pods, all scheduled on that node with 14 debugger sidecars.
The run also requires exactly 14 DDB sessions. A mismatch stops the recipe
before any latency samples are collected.

The fixed SocialNet revision is
`613f316ca060b94545e850324f91eef1ceb7639b`. Full runs use three preparation
cycles and 30 same-pause commands per depth. The first same-pause command is
hidden and excluded, leaving 29 reported warm samples.

Do not edit generated files under `runtime/` or `results/`. Setup renders all
DDB and SocialNet configuration from the checked-in templates. The DDB template
loads `/workspace/extension.py` first and then
`/workspace/runtime-serviceweaver.py`.

## 2. Run

```bash
./artifact.sh setup
./artifact.sh config
./artifact.sh check
./artifact.sh smoke
./artifact.sh run
./artifact.sh results
```

`setup` installs any missing Kubernetes tools, builds and deploys SocialNet,
pins all 14 processes to the sole node, seeds the application, builds DDB,
creates the private SSH gateway, injects debugger sidecars, and renders the
DDB configuration. It builds the graph seeder with the checked-in
random-number race fix; the measured SocialNet server is unchanged.

`config` is an optional read-only report of what setup resolved. It does not
install or configure anything.

The experiment calls `read-user-timeline` and uses these fixed breakpoints:

| RPC depth | Breakpoint |
|---:|---|
| 1 | `backend_service.go:245` |
| 2 | `user_timeline_service.go:28` |
| 3 | `storage.go:263` |

See [METHODOLOGY.md](METHODOLOGY.md) for the timing boundary, kernel-stop
verification, sample validation, and aggregation rules.

## 3. Results

Results are stored under `results/`. `results/latest` points to the newest run.
The main table is `call-depth-summary.csv`; per-depth samples, DDB logs, trigger
logs, boundary counts, and kernel evidence are stored beside it.

Reference result from 29 reported warm DBTs per depth:

| RPC depth | Steady samples | Mean | Median | P95 |
|---:|---:|---:|---:|---:|
| 1 | 29 | 86.694 ms | 86.769 ms | 87.823 ms |
| 2 | 29 | 130.136 ms | 129.778 ms | 131.326 ms |
| 3 | 29 | 175.885 ms | 175.666 ms | 178.396 ms |

Linear fit: `latency_ms = 41.714 + 44.596 * RPC_depth`.

## 4. Cleanup

Remove debugger resources and experiment placement while leaving SocialNet
running and reseeded:

```bash
./artifact.sh restore --yes
```

The cleanup does not modify node taints.
