# ServiceWeaver SocialNet Call-Depth Experiment

Measure DDB backtrace latency at call depths 2, 3, 4, 5, 6, and 10.

## Requirements

- One physical target host with enough capacity for all 14 SocialNet processes.
  The Kubernetes cluster may contain additional nodes.
- Linux with systemd and passwordless or interactive `sudo` access.
- `python3`, `curl`, `patch`, Docker, Cargo, Go, and Git.
- Registry access for the build, debugger, and gateway images
- SocialNet commit `613f316ca060b94545e850324f91eef1ceb7639b`

`setup` checks and, when missing, installs native k3s `v1.36.2+k3s1`,
`kubectl` `v1.36.2`, and `weaver-kube` `v0.23.0`. The k3s installation uses
the official installer and may prompt for the sudo password. Existing active
native-k3s installations are reused.

Run every command on the k3s controller. All experiment configuration and test
code is in this directory; only DDB Rust and ServiceWeaver SocialNet are
external source inputs.

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
Before measuring, `setup` fixes all 14 SocialNet deployments at one replica and
pins them to `TARGET_NODE`. `check` requires exactly 14 Ready SocialNet pods on
that one node with 14 debugger sidecars. Other Kubernetes nodes may remain
joined, but no measured application process may run on them. The run also
requires exactly 14 DDB sessions. A mismatch stops the recipe before any
latency samples are collected.

The fixed SocialNet revision is
`613f316ca060b94545e850324f91eef1ceb7639b`. Full runs use three preparation
cycles and 10 same-pause commands per depth. The first same-pause command is
hidden and excluded, leaving 9 reported warm samples.

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

After updating the SocialNet source on an already configured cluster, replace
the running application once with:

```bash
./artifact.sh setup --rebuild-app
```

`setup` installs any missing Kubernetes tools, builds and deploys SocialNet,
fixes each process at one replica, pins all 14 processes to `TARGET_NODE`,
seeds the application, builds DDB, creates the private SSH gateway, injects
debugger sidecars, and renders the DDB configuration. It builds the graph
seeder with the checked-in random-number race fix.

An existing command-latency cluster does not need to be torn down. Run
call-depth setup on its controller; setup reuses the deployed application and
reconfigures only its replicas and placement:

```bash
cd ../sw-socialnet-call-depth
./artifact.sh setup
```

To switch back, rerun command-latency setup with the application build and
deployment steps skipped. It restores the replica count and worker placement
from that recipe's configuration:

```bash
cd ../sw-socialnet-command-latency
./artifact.sh setup --skip-app-build --skip-app-deploy
```

`config` is an optional read-only report of what setup resolved. It does not
install or configure anything.

The experiment calls `read-user-timeline` and uses these fixed breakpoints:

| Call depth | RPC boundaries | Breakpoint |
|---:|---:|---|
| 2 | 1 | `backend_service.go:245` |
| 3 | 2 | `user_timeline_service.go:28` |
| 4 | 3 | `call_depth_service.go:64` |
| 5 | 4 | `call_depth_service.go:68` |
| 6 | 5 | `call_depth_service.go:72` |
| 10 | 9 | `storage.go:263` |

The request follows one synchronous Service Weaver path:

```text
Main -> Backend -> UserTimeline -> Relay1 -> Relay2 -> Relay3 -> Relay4 -> Relay5 -> Relay6 -> Storage
```

The relays are colocated with existing component groups, so the extended path
does not change the 14-process deployment.

See [METHODOLOGY.md](METHODOLOGY.md) for the timing boundary, kernel-stop
verification, sample validation, and aggregation rules.

## 3. Results

Results are stored under `results/`. `results/latest` points to the newest run.
The main table is `call-depth-summary.csv`; per-depth samples, DDB logs, trigger
logs, boundary counts, and kernel evidence are stored beside it.

Each run prints the six-depth table and its linear fit. The first DBT at every
depth primes the same-pause command path; the remaining 9 samples appear in
`call-depth-summary.csv`.

## 4. Cleanup

Remove debugger resources and experiment placement while leaving SocialNet
running and reseeded:

```bash
./artifact.sh restore --yes
```

The cleanup does not modify node taints.
