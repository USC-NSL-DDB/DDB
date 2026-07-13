# DDB Experiments

Reproduction harnesses for the paper's evaluation. Each experiment is self-contained in its own directory with its own README as the guide.

This doc goes over shared part of setting up DDB and needed components for these experiments. You may need to perform such setup on every newly provisioned testbed, either CloudLab or Chameleon.

## The experiments

| Directory | Measures | Nodes |
|---|---|---|
| [`overhead/nu-socialnet-overhead`](overhead/nu-socialnet-overhead/) | DDB attach overhead on Nu's socialNetwork (+ decomposition vs vanilla Nu) | 5 |
| [`overhead/sw-socialnet-overhead`](overhead/sw-socialnet-overhead/) | DDB overhead on ServiceWeaver socialnet on k3s | 5 |
| [`overhead/raft-overhead`](overhead/raft-overhead/) | debugger overhead on a 3-node Raft cluster (none / DDB / gdb-MI) | 4 |
| [`overhead/faketime-overhead`](overhead/faketime-overhead/) | per-call cost of libfaketime time-API interposition | 1 |
| [`pet-perceived-gap`](pet-perceived-gap/) | wall-clock time an app perceives across a DDB-compensated pause | 1 | 

## Cluster assumptions

For four overhead and the PET-perceived gap experiments, we use CloudLab as the testbed.

### CloudLab setup assumptions

- **node0 is the head node**. Every command is run from it; it hosts the load generator, DDB, and the MQTT broker. The other nodes run the system under test. Node addressing is over the experiment network (`10.10.x.y`, node0 =`.1`), hard-coded in each experiment harness's `common.sh` / `cluster.txt`.
- **Keep free space on `/`**. These images ship a 16G root disk; journald plus broker state can fill it under benchmark churn, and a full `/` kills DDB mid-run (harnesses detect this and reject the run, but you lose the trial). `sudo journalctl --vacuum-size=100M` reclaims space safely. Big artifacts (gRPC install, build temporaries) belong on the large local disk (`/mnt/local`), and harnesses already pu them there. You may also run `free_disk_cloudlab.sh` (from this dir) to reclaim some spaces on CloudLab machines.

## Installing DDB on the head/control node

### Dependencies
```bash
# rust, if missing
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# build (from the repo root)
cd scripts
./install.sh
```

## Docker + the managed MQTT broker

DDB's service discovery runs over MQTT. With `managed: type: emqx` in the DDB
config (what the harnesses use), DDB starts an **EMQX broker in a Docker
container on node0** — so node0 needs Docker and your user in the `docker`
group:

```bash
sudo usermod -aG docker "$USER"
newgrp docker   # usermod does NOT affect the current shell -- run this
docker ps 	# must succeed without sudo
```

## Layout

```
exp/
├── README.md
├── overhead/
│   ├── nu-socialnet-overhead/
│   ├── sw-socialnet-overhead/
│   ├── raft-overhead/
│   └── faketime-overhead/
└── pet-perceived-gap/
```
