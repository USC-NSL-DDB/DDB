# DDB Experiments

Reproduction harnesses for the paper's evaluation. Each experiment is self-contained in its own directory with its own README as the guide.

This doc goes over shared part of setting up DDB and needed components for these experiments. You may need to perform such setup on every newly provisioned testbed, either CloudLab or Chameleon.

## The experiments

| Directory | Measures | Nodes |
|---|---|---|
| [`overhead/nu-socialnet-overhead`](overhead/nu-socialnet-overhead/) | DDB attach overhead on Nu's socialNetwork (+ decomposition vs vanilla Nu) | 5 |
| [`overhead/sw-socialnet-overhead`](overhead/sw-socialnet-overhead/) | DDB overhead on ServiceWeaver socialnet on k3s | 5 |
| [`overhead/sw-socialnet-call-depth`](overhead/sw-socialnet-call-depth/) | DDB `dbt` latency at RPC depths 1, 2, and 3 | 1 |
| [`overhead/sw-socialnet-command-latency`](overhead/sw-socialnet-command-latency/) | Warm DDB `dbt` latency for every application thread while all SocialNet processes remain stopped | 10 |
| [`overhead/raft-overhead`](overhead/raft-overhead/) | debugger overhead on a 3-node Raft cluster (none / DDB / gdb-MI) | 4 |
| [`overhead/faketime-overhead`](overhead/faketime-overhead/) | per-call cost of libfaketime time-API interposition | 1 |
| [`pet-perceived-gap`](pet-perceived-gap/) | wall-clock time an app perceives across a DDB-compensated pause | 1 | 

For call depth, `1` means that all application processes run on one target
node; additional Chameleon nodes may remain joined to the cluster.

## Cluster assumptions

Four of the overhead experiments and the PET-perceived-gap experiment use
CloudLab. The ServiceWeaver call-depth and command-latency experiments use
Chameleon.

### CloudLab setup assumptions

- **node0 is the head node**. Every command is run from it; it hosts the load generator, DDB, and the MQTT broker. The other nodes run the system under test. Node addressing is over the experiment network (`10.10.x.y`, node0 =`.1`), hard-coded in each experiment harness's `common.sh` / `cluster.txt`.
- **Keep free space on `/`**. These images ship a 16G root disk; journald plus broker state can fill it under benchmark churn, and a full `/` kills DDB mid-run (harnesses detect this and reject the run, but you lose the trial). `sudo journalctl --vacuum-size=100M` reclaims space safely. Big artifacts (gRPC install, build temporaries) belong on the large local disk (`/mnt/local`), and harnesses already put them there. You may also run `free_disk_cloudlab.sh` (from this directory) to reclaim space on CloudLab machines.

### Chameleon setup assumptions

- **Provision ten or more instances on the same private network.** Designate one instance as the controller and use the remaining instances as workers. Run recipe commands from the controller; each experiment README specifies the nodes that its topology uses.
- **Use the same SSH key pair on every instance.** The controller must reach the workers directly and non-interactively over their private IP addresses. All instances need Internet access, and the security group must permit SSH access to the controller and communication among the instances.
- **Assign a floating IP only to the controller.** Use it to connect from your local machine; the experiment recipes use private IP addresses for cluster communication.
- **Install Docker, Cargo, and Go on the controller.** Follow the installation instructions below; the recipe scripts handle the remaining cluster and experiment setup.
- **Disable `firewalld` on the controller and every worker used by a recipe**, or configure it to permit the required k3s and Flannel traffic over the private network:

```bash
sudo systemctl disable --now firewalld
```

Follow the call-depth or command-latency experiment README linked in the table
above for its specific topology and run instructions.

## Installing DDB on the head/control node

### Rust and Cargo

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

### Go （Skip for CloudLab Setup)

Install Go using either option below.

**Via Snap:**

```bash
sudo snap install go --classic
```

**Via the [official tarball](https://go.dev/dl/) (Go 1.26.4, Linux amd64):**

```bash
wget https://go.dev/dl/go1.26.4.linux-amd64.tar.gz -O /tmp/go1.26.4.linux-amd64.tar.gz && sudo rm -rf /usr/local/go && sudo tar -C /usr/local -xzf /tmp/go1.26.4.linux-amd64.tar.gz && sudo ln -sf /usr/local/go/bin/go /usr/local/bin/go && sudo ln -sf /usr/local/go/bin/gofmt /usr/local/bin/gofmt
```

### Build DDB

```bash
# From the repository root
cd scripts
./install.sh
```

## Docker + the managed MQTT broker

DDB's service discovery runs over MQTT. With `managed: type: emqx` in the DDB
config (what the harnesses use), DDB starts an **EMQX broker in a Docker
container on the head/controller node**.

### CloudLab

The baked DDB CloudLab profile already includes Docker. No Docker installation
is needed.

### Chameleon

Install Docker Engine on the controller from the
[official Docker apt repository](https://docs.docker.com/engine/install/ubuntu/):

```bash
sudo apt update
sudo apt install -y ca-certificates curl
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
sudo chmod a+r /etc/apt/keyrings/docker.asc

sudo tee /etc/apt/sources.list.d/docker.sources >/dev/null <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: $(. /etc/os-release && echo "${UBUNTU_CODENAME:-$VERSION_CODENAME}")
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF

sudo apt update
sudo apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
```

### Run Docker without `sudo`

On either testbed, add your user to the `docker` group and verify access:

```bash
sudo usermod -aG docker "$USER"
newgrp docker   # usermod does NOT affect the current shell -- run this
docker ps       # must succeed without sudo
```

## Layout

```
exp/
├── README.md
├── overhead/
│   ├── nu-socialnet-overhead/
│   ├── sw-socialnet-overhead/
│   ├── sw-socialnet-call-depth/
│   ├── sw-socialnet-command-latency/
│   ├── raft-overhead/
│   └── faketime-overhead/
└── pet-perceived-gap/
```
