# ServiceWeaver SocialNet Command-Latency Experiment

Measure warm DDB `dbt` latency for every application thread while all
SocialNet processes remain stopped.

## Requirements

Use Chameleon instances on one private network:

- one controller, where this recipe, DDB, and the experiment driver run;
- one or more workers reachable directly by SSH from the controller;
- Linux with systemd on every instance;
- passwordless `sudo` on the workers and `sudo` access on the controller.

The controller must already have `python3`, `curl`, `patch`, Docker, Cargo, Go,
Git, and an OpenSSH client. Workers need `curl`, `ip`, and systemd. Setup checks
and installs the following when absent:

- native k3s `v1.36.2+k3s1` server on the controller;
- the matching native k3s agents on every configured worker;
- `kubectl` `v1.36.2` on the controller;
- `weaver-kube` `v0.23.0` on the controller.

All instances need registry and Internet access during setup. The fixed
SocialNet source revision is
`613f316ca060b94545e850324f91eef1ceb7639b`.

## 1. Configure the cluster

Run all commands from this directory on the controller. Create the two local
configuration files:

```bash
cp artifact.env.example artifact.env
cp workers.txt.example workers.txt
```

In `artifact.env`, set `CONTROLLER_IP` to the controller's private Chameleon
address. Set `SSH_IDENTITY_FILE` only when the controller's normal SSH
configuration cannot reach the workers.

Set `SOCIALNET_REPLICAS` to the number of replicas for each of SocialNet's 14
deployments. It defaults to one; for example, two replicas produce 28
application processes and 28 DDB sessions:

```bash
SOCIALNET_REPLICAS=2
```

Rerun `./artifact.sh setup` after changing this value. Setup reconciles the
autoscalers, deployments, debugger sidecars, and DDB session count.

In `workers.txt`, enter one SSH target per worker. The recipe derives its
cluster size from this inventory:

```text
cc@<worker-1-private-address>
cc@<worker-2-private-address>
cc@<worker-3-private-address>
cc@<worker-4-private-address>
# Add or remove lines to match the workers allocated to this run.
```

Any nonempty worker inventory is accepted. Kubernetes distributes the
SocialNet processes across the available workers.

The worker addresses must be reachable from the controller without a jump
host, and SSH must work non-interactively. Verify the resolved configuration:

```bash
./artifact.sh config
```

The recipe discovers worker node IPs and interfaces from their route to
`CONTROLLER_IP`; do not configure Kubernetes node names or worker interfaces.

Chameleon images may start `firewalld` with only SSH allowed. Setup stops
before changing the cluster when it detects this. Either permit the k3s
control-plane, kubelet, and Flannel traffic on the private network, then set
`ALLOW_ACTIVE_FIREWALL=1`, or explicitly disable firewalld on the controller
and every configured worker:

```bash
sudo systemctl disable --now firewalld
```

Run that command locally on the controller and through SSH on each worker.
Disabling it also removes filtering from any public interface, so use Chameleon
security groups or configure firewalld when public exposure is a concern.

## 2. Run the recipe

```bash
./artifact.sh setup
./artifact.sh check
./artifact.sh smoke
./artifact.sh run
./artifact.sh results
```

`setup` performs the complete workflow:

1. installs or validates the pinned k3s server, agents, `kubectl`, and
   `weaver-kube`;
2. forms the inventory-sized cluster and taints the controller;
3. builds DDB and the accepted SocialNet source;
4. deploys and distributes the configured SocialNet replicas;
5. seeds the social graph;
6. creates the private SSH gateway, injects one debugger sidecar per process,
   and renders the DDB configuration;
7. runs the cluster and attachment preflight.

Setup applies non-persistent controller TCP settings needed by the graph
seeder. It does not add packages other than the Kubernetes tools listed above.
If a worker already contains a k3s *server* service, setup refuses to erase it
and prints the explicit cleanup command instead.

### Switch from call depth

The existing cluster and SocialNet deployment can be reused. From the
command-latency directory, rerun setup without rebuilding or redeploying the
application:

```bash
./artifact.sh setup --skip-app-build --skip-app-deploy
```

Setup reads `workers.txt` and `SOCIALNET_REPLICAS`, restores worker-only
placement and topology spreading, and prepares the command-latency debugger
configuration. Calling call-depth `restore` first is unnecessary.

`smoke` runs one excluded warm-up batch followed by two measured batches on one
thread. `run` uses one excluded warm-up batch followed by 30 measured batches.
Every batch submits exactly one DBT for every discovered thread, waits for the
entire batch to complete, and only then starts the next batch. Threads are
ordered round-robin across the DDB sessions so DDB's command workers can run
requests for different processes concurrently. DDB pauses the entire
application once; no process is continued between batches. Kernel state is
checked before sampling and after DDB exits.

The DDB template loads `/workspace/extension.py` first and then
`/workspace/runtime-serviceweaver.py`. Generated files under `runtime/` and
`results/` should not be edited.

## 3. Results

Each run is stored under `results/`, and `results/latest` points to the newest
run. `summary.csv` contains the aggregate table. The run directory also keeps
raw samples, per-thread and per-depth summaries, thread-to-process mappings,
DDB logs, and kernel tracer evidence.

See [METHODOLOGY.md](METHODOLOGY.md) for the timing boundary and validation
rules.
