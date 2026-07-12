# DDB Experiments

Reproduction harnesses for the paper's evaluation. Each experiment is
self-contained in its own directory with its own README as the authoritative
guide; **this file collects what they share** — cluster assumptions, DDB and
dependency installation, and the measurement conventions every harness follows
— so the per-experiment READMEs can stay focused on what is specific to them.

## The experiments

| Directory | Measures | Nodes | Extra deps beyond this page |
|---|---|---|---|
| [`overhead/nu-socialnet-overhead`](overhead/nu-socialnet-overhead/) | DDB attach overhead on Nu's socialNetwork (+ decomposition vs vanilla Nu) | 5 | Caladan NIC (Mellanox), hugepages/ksched |
| [`overhead/sw-socialnet-overhead`](overhead/sw-socialnet-overhead/) | DDB overhead on ServiceWeaver socialnet on k3s | 5 | k3s, Go, weaver-kube |
| [`overhead/raft-overhead`](overhead/raft-overhead/) | debugger overhead on a 3-node Raft cluster (none / DDB / gdb-MI) | 4 | **private repo** (see its README), DDB-patched gRPC, gcc-13 |
| [`overhead/faketime-overhead`](overhead/faketime-overhead/) | per-call cost of libfaketime time-API interposition | 1 | none |
| [`pet-perceived-gap`](pet-perceived-gap/) | wall-clock time an app perceives across a DDB-compensated pause | 1 | libfaketime, gdb ≥ python support |

## Cluster assumptions

All multi-node experiments were built for CloudLab-style clusters and share the
same shape:

- **node0 is the head node** — every command is run from it; it hosts the load
  generator, DDB, and the MQTT broker. The other nodes run the system under
  test. Node addressing is over the experiment network (`10.10.x.y`, node0 =
  `.1`), hard-coded in each harness's `common.sh` / `cluster.txt`.
- **Passwordless SSH and passwordless `sudo`** from node0 to every node
  (including node0 itself). Every harness checks this up front.
- **No shared filesystem** is assumed: harnesses `rsync`/`scp` binaries to the
  worker nodes themselves (`setup_nodes.sh` / `deploy_all.sh`).
- **Keep free space on `/`.** These images ship a 16G root disk; journald plus
  broker state can fill it under benchmark churn, and a full `/` kills DDB
  mid-run (harnesses detect this and reject the run, but you lose the trial).
  `sudo journalctl --vacuum-size=100M` reclaims space safely. Big artifacts
  (gRPC install, build temporaries) belong on the large local disk
  (`/mnt/local`), and harnesses already put them there.
- **Toolchain trap:** these images ship an experimental `g++-13` (13.0.0, no
  `<format>`) in `/usr/local/bin` that shadows the real one on `PATH`.
  Harnesses that need C++20 install the packaged gcc-13 and always invoke it
  by absolute path (`/usr/bin/g++-13`); don't "fix" the system alternatives —
  other experiments depend on the default compiler staying put.

## Installing DDB (common to every DDB-attached experiment)

DDB itself — the Rust orchestrator that discovers debuggees, ssh'es to their
nodes and drives one gdb per process:

```bash
# rust, if missing
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# build (from the repo root; produces ddb/target/release/ddb)
cargo build --release --manifest-path ddb/Cargo.toml
```

**Connector headers** — applications report themselves to DDB through a small
header-only library (`connector/`). Frameworks compiled against it (Nu with
`CONFIG_DDB=y`, the raft lab, the DDB-patched gRPC) need it installed:

```bash
make -C connector install                      # -> ~/.local/include/ddb
make -C connector PREFIX=/some/prefix install  # or into a custom prefix
```

**gdb** must be installed on every node that runs a debuggee (DDB ssh'es in
and attaches one gdb per process; attaching to a running process needs root on
these images — `kernel.yama.ptrace_scope=1` — which is why DDB runs its gdb
under `sudo`).

## Docker + the managed MQTT broker

DDB's service discovery runs over MQTT. With `managed: type: emqx` in the DDB
config (what the harnesses use), DDB starts an **EMQX broker in a Docker
container on node0** — so node0 needs Docker and your user in the `docker`
group:

```bash
sudo usermod -aG docker "$USER"
newgrp docker        # usermod does NOT affect the current shell -- run this
docker info          # must succeed without sudo
```

The `newgrp` step is the single most common setup failure: a shell that
predates the group membership fails with "cannot talk to docker" even though
`/etc/group` is correct.

How discovery flows in the connector-based harnesses (nu, raft):

1. DDB starts, launches the broker, and writes the broker address into
   `/tmp/ddb/service_discovery/config`.
2. The harness copies that file to every debuggee node (each connector reads
   it locally to find the broker).
3. Each debuggee starts with its DDB flag (e.g. `--ddb --ddb_node_ip <its own
   ip>`), reports itself, and **parks in `sigwait()` until DDB attaches and
   releases it** — which is what makes attach race-free: a debuggee never does
   real work before its debugger is in place.
4. The harness resumes sessions explicitly (`-exec-continue`), in whatever
   order the system under test requires.

(The ServiceWeaver experiment is the exception: on k3s, discovery and attach go
through injected debug sidecars and an ssh-gateway instead of the sigwait
handshake — see its README.)

Mind that the address the debuggee reports (`--ddb_node_ip` /
`--ddb_host_ip`) is **its own IP** — it is where DDB ssh'es back to attach,
not the broker's or the client's address.

## libfaketime (time-virtualization experiments)

`pet-perceived-gap` and `overhead/faketime-overhead` use the repo's own copy —
build it from source, don't use a distro package:

```bash
make -C libfaketime/src all    # -> libfaketime/src/libfaketime.so.1
```

## Measurement conventions

Every harness in this tree follows the same rules; if you add an experiment,
follow them too:

- **One invocation = one data point.** Each run brings the system up from
  scratch, measures once, and tears everything down (`stop_all.sh` cleans up
  by hand after an interrupted run). Runs vary a little; take **≥3 trials per
  configuration and compare medians**, interleaving configurations rather than
  running them in blocks.
- **Every run verifies its own premise, before *and after* measuring.**
  Debugger modes check `TracerPid` on every debuggee (and that DDB survived
  the whole measurement); baselines check nothing is attached; interposition
  experiments check the library is really mapped. The failure this prevents is
  the worst one an overhead experiment can have: a silently missing debugger
  measures the baseline twice and reports "zero overhead".
- **One run at a time.** Harnesses `flock` a run lock; two concurrent
  invocations would kill each other's processes.
- **Pick the operating point deliberately.** Overhead is load-dependent: at
  low load it can be invisible (latency floors hide it), and past saturation
  it dilutes. The per-experiment READMEs document where their knee is and why
  the default load sits there.
- Results land in each experiment's `results/` (gitignored) as
  timestamped files; reference numbers and their interpretation live in the
  experiment's README.

## Layout

```
exp/
├── README.md                     <- you are here: shared setup + conventions
├── overhead/
│   ├── nu-socialnet-overhead/    <- per-experiment README is authoritative
│   ├── sw-socialnet-overhead/
│   ├── raft-overhead/
│   └── faketime-overhead/
└── pet-perceived-gap/
```
