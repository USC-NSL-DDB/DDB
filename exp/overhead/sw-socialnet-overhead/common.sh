#!/bin/bash
# shellcheck shell=bash
#
# Shared helpers for the sw-socialnet-overhead experiment scripts.
# Source this from any script in this directory:
#
#   source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Ask git for the repo root rather than counting `..` levels: this directory has
# already been moved once, which is what silently broke the old relative paths.
REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [[ -z "$REPO_ROOT" ]]; then
  REPO_ROOT="$(cd "$EXP_DIR/../../.." && pwd)"
fi
SOCIALNET_DIR="$REPO_ROOT/fwks/socialnetwork"

MASTER_IP="${MASTER_IP:-10.10.1.1}"
K3S_KUBECONFIG="/etc/rancher/k3s/k3s.yaml"
K3S_CONFIG="/etc/rancher/k3s/config.yaml"
K3S_CONFIG_MARKER="# managed by sw-socialnet-overhead"
APP_LABEL_KEY="serviceweaver/app"
NAMESPACE="${NAMESPACE:-default}"

# Name a node gets in k8s, derived from its experiment IP: 10.10.1.N -> node<N-1>.
# The CloudLab images leave several workers with the hostname "localhost", so
# letting k3s default the node name to the hostname makes them collide on a
# single node object and silently overwrite each other.
node_name_for_ip() {
  local last="${1##*.}"
  echo "node$((last - 1))"
}

# The NIC carrying the 10.10.1.x experiment network. Nodes must register on this
# interface, not the public one, or flannel builds its VXLAN over the public
# addresses and pod-to-pod traffic across nodes is dropped.
cluster_iface() {
  local pfx="${MASTER_IP%.*}."
  ip -o -4 addr show | awk -v pfx="$pfx" 'index($4, pfx) == 1 { print $2; exit }'
}

# Go installs to /usr/local/go/bin and `go install` drops binaries (weaver,
# weaver-kube) in $HOME/go/bin. Non-interactive shells never source ~/.bashrc,
# so put both on PATH explicitly.
export PATH="/usr/local/go/bin:$HOME/go/bin:$PATH"

die() {
  echo "Error: $*" >&2
  exit 1
}

# Both ./build_app.sh and weaver-kube (which shells out to `docker build`) need
# the daemon reachable without sudo.
ensure_docker() {
  command -v docker >/dev/null || die "docker not found on PATH. Run ./deploy_all.sh first."
  docker info >/dev/null 2>&1 && return 0
  die "cannot talk to the docker daemon as $(whoami).
  Add yourself to the docker group, then start a new login shell:
    sudo usermod -aG docker \$(whoami)
    newgrp docker"
}

host_uses_cgroup_v1() {
  [[ "$(stat -fc %T /sys/fs/cgroup)" != "cgroup2fs" ]]
}

# Desired /etc/rancher/k3s/config.yaml for the master. Three things matter:
#   fail-cgroupv1  Kubernetes >= 1.35 refuses to start a kubelet on cgroup v1,
#                  and get.k3s.io installs the latest k3s on these cgroup v1
#                  images, so without this the server crash-loops.
#   node-ip        pin the InternalIP to the experiment network.
#   flannel-iface  build the VXLAN overlay on the experiment NIC, not the
#                  public one (cross-node pod traffic is dropped otherwise).
desired_k3s_config() {
  local iface="$1"
  echo "$K3S_CONFIG_MARKER"
  if host_uses_cgroup_v1; then
    printf 'kubelet-arg:\n  - "fail-cgroupv1=false"\n'
  fi
  printf 'node-ip: "%s"\nflannel-iface: "%s"\n' "$MASTER_IP" "$iface"
}

ensure_k3s_config() {
  local iface desired current
  iface="$(cluster_iface)"
  [[ -n "$iface" ]] || die "no interface carries ${MASTER_IP%.*}.x on this host"

  desired="$(desired_k3s_config "$iface")"
  current="$(sudo cat "$K3S_CONFIG" 2>/dev/null || true)"

  [[ "$current" == "$desired" ]] && return 0

  if [[ -n "$current" ]] && ! grep -qF "$K3S_CONFIG_MARKER" <<<"$current"; then
    die "$K3S_CONFIG exists and was not written by these scripts.
  Merge these keys into it by hand, then re-run:
$(desired_k3s_config "$iface" | sed 's/^/    /')"
  fi

  echo "Writing $K3S_CONFIG (node-ip=$MASTER_IP flannel-iface=$iface)"
  sudo mkdir -p "$(dirname "$K3S_CONFIG")"
  printf '%s\n' "$desired" | sudo tee "$K3S_CONFIG" >/dev/null
  K3S_NEEDS_RESTART=1
}

# Flags a worker needs on `k3s agent`. $1 = the worker's experiment IP,
# $2 = the NIC carrying that IP on the worker.
k3s_agent_args() {
  local ip="$1" iface="$2"
  local args="--node-name $(node_name_for_ip "$ip") --node-ip $ip --flannel-iface $iface"
  if host_uses_cgroup_v1; then
    args+=" --kubelet-arg=fail-cgroupv1=false"
  fi
  echo "$args"
}

# The node names this cluster should end up with: the master's hostname plus
# node1..nodeN derived from cluster.txt.
expected_node_names() {
  local ip
  echo "$(hostname)"
  while read -r ip || [[ -n "$ip" ]]; do
    [[ -z "$ip" || "$ip" == \#* || "$ip" == "$MASTER_IP" ]] && continue
    node_name_for_ip "$ip"
  done < "${1:-$EXP_DIR/cluster.txt}"
}

# Wait until every expected node exists AND is Ready.
#
# Checking only "no node is NotReady" is not enough: agents register a few
# seconds apart, so early on the API server honestly reports zero NotReady nodes
# simply because the other workers have not shown up yet.
wait_for_nodes() {
  local cluster_file="${1:-$EXP_DIR/cluster.txt}"
  local attempts="${2:-40}"
  local expected
  mapfile -t expected < <(expected_node_names "$cluster_file")

  echo "Waiting for ${#expected[@]} nodes to register and become Ready..."
  for i in $(seq 1 "$attempts"); do
    local ready=0 missing=()
    for n in "${expected[@]}"; do
      if [[ "$(kubectl get node "$n" -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null)" == "True" ]]; then
        ready=$((ready + 1))
      else
        missing+=("$n")
      fi
    done
    if [[ "$ready" -eq "${#expected[@]}" ]]; then
      echo "All ${#expected[@]} nodes Ready."
      return 0
    fi
    echo "  $ready/${#expected[@]} Ready; waiting on: ${missing[*]} ($i/$attempts)"
    sleep 5
  done

  die "timed out waiting for nodes: ${missing[*]}
  Check a failing worker with: ssh <ip> tail -20 /tmp/k3s-agent.log"
}

# Remove node objects that are not the master and not node1..nodeN. The
# hostname-collision bug leaves behind a stale "localhost" node.
prune_stale_nodes() {
  local expected=()
  mapfile -t expected < <(expected_node_names "${1:-$EXP_DIR/cluster.txt}")

  local node
  while read -r node; do
    [[ -z "$node" ]] && continue
    local keep=0
    for e in "${expected[@]}"; do
      [[ "$node" == "$e" ]] && keep=1
    done
    if [[ "$keep" -eq 0 ]]; then
      echo "Removing stale node object: $node"
      kubectl delete node "$node" --ignore-not-found >/dev/null
    fi
  done < <(kubectl get nodes -o name --no-headers 2>/dev/null | sed 's|^node/||')
}

# Keep app pods off the master: it runs the load generator, and co-scheduling
# microservices there would contaminate the measurements.
taint_master() {
  kubectl taint node "$(hostname)" \
    node-role.kubernetes.io/control-plane=:NoSchedule --overwrite >/dev/null
  echo "Master $(hostname) tainted NoSchedule (no app pods)."
}

k3s_server_up() {
  systemctl is-active --quiet k3s && sudo k3s kubectl get nodes >/dev/null 2>&1
}

# A running server can predate a config change: config.yaml may already say
# node-ip: 10.10.1.1 while the live process still advertises the public IP.
master_registered_correctly() {
  local addr
  addr="$(sudo k3s kubectl get node "$(hostname)" \
    -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}' 2>/dev/null)"
  [[ "$addr" == "$MASTER_IP" ]]
}

# The k3s server must be up on the master before any kubectl call works. The
# unit is installed (and enabled) by install_deps_all.sh, but a fresh boot, a
# crash-loop, or a stray `systemctl stop k3s` leaves it down.
ensure_k3s_server() {
  systemctl cat k3s.service >/dev/null 2>&1 \
    || die "k3s is not installed on this node. Run ./deploy_all.sh first."

  K3S_NEEDS_RESTART=0
  ensure_k3s_config

  if [[ "$K3S_NEEDS_RESTART" -eq 0 ]] && k3s_server_up && master_registered_correctly; then
    return 0
  fi

  if k3s_server_up && ! master_registered_correctly; then
    echo "Master advertises the wrong InternalIP; restarting k3s to apply $K3S_CONFIG"
  fi

  echo "Starting k3s server..."
  sudo systemctl restart k3s

  # `is-active` only reports active once k3s notifies systemd it is up, so a
  # crash-looping server (state "activating") never passes this check.
  for i in $(seq 1 45); do
    if k3s_server_up && master_registered_correctly; then
      echo "k3s API server ready (InternalIP $MASTER_IP)."
      return 0
    fi
    sleep 2
  done

  die "k3s server did not become ready after 90s.
  Inspect it with: sudo journalctl -u k3s -n 50 --no-pager"
}

# k3s writes a root-only kubeconfig. Make a user-owned copy so kubectl (and the
# python kubernetes client) work without sudo.
ensure_kubeconfig() {
  if [[ -n "${KUBECONFIG:-}" && -r "$KUBECONFIG" ]]; then
    return 0
  fi

  local user_cfg="$HOME/.kube/config"
  if [[ ! -r "$user_cfg" ]]; then
    [[ -f "$K3S_KUBECONFIG" ]] || die "$K3S_KUBECONFIG not found. Is the k3s server running on this node?"
    echo "Copying k3s kubeconfig to $user_cfg ..."
    mkdir -p "$HOME/.kube"
    sudo cat "$K3S_KUBECONFIG" > "$user_cfg"
    chmod 600 "$user_cfg"
  fi
  export KUBECONFIG="$user_cfg"
}

# Path to a kubeconfig readable by the current user (for tools that take a path).
kubeconfig_path() {
  ensure_kubeconfig
  echo "$KUBECONFIG"
}

# Name of the apilistener service ("service/apilistener-xxxx"), or empty.
# awk (not `grep | head`) so an early pipe close can't trip `set -o pipefail`.
apilistener_svc() {
  kubectl get svc -n "$NAMESPACE" -o name 2>/dev/null | awk '/apilistener/ && !seen++'
}

app_is_deployed() {
  [[ -n "$(apilistener_svc)" ]]
}

# ServiceWeaver app label value (e.g. "server.out").
app_label_value() {
  kubectl get deployments -n "$NAMESPACE" -l "$APP_LABEL_KEY" \
    -o jsonpath="{.items[*].metadata.labels.$APP_LABEL_KEY}" 2>/dev/null \
    | tr ' ' '\n' | sort -u | awk 'NF && !seen++'
}

patch_nodeport() {
  local svc
  svc="$(apilistener_svc)"
  [[ -n "$svc" ]] || die "no apilistener service found. Deploy the app first (./deploy_app.sh)."
  kubectl patch "$svc" -n "$NAMESPACE" -p '{"spec":{"type":"NodePort"}}' >/dev/null
  echo "Endpoint: $(detect_endpoint)"
}

detect_endpoint() {
  local svc node_port
  svc="$(apilistener_svc)"
  [[ -n "$svc" ]] || die "no apilistener service found. Run ./setup_experiment.sh first."
  node_port="$(kubectl get "$svc" -n "$NAMESPACE" -o jsonpath='{.spec.ports[0].nodePort}')"
  [[ -n "$node_port" ]] || die "apilistener is not a NodePort service. Run ./setup_experiment.sh first."
  echo "http://${MASTER_IP}:${node_port}"
}

# Wait until every app pod (ignoring ssh-gateway) is Ready.
wait_for_pods() {
  local attempts="${1:-60}"
  echo "Waiting for all app pods to be Ready..."
  for i in $(seq 1 "$attempts"); do
    local not_ready
    not_ready=$(kubectl get pods -n "$NAMESPACE" --no-headers 2>/dev/null \
      | grep -v "ssh-gateway" \
      | grep -cv "1/1.*Running" || true)
    if [[ "$not_ready" -eq 0 ]]; then
      echo "All app pods Ready."
      return 0
    fi
    echo "  $not_ready pod(s) not ready... ($i/$attempts)"
    sleep 5
  done
  echo "Warning: some pods still not ready" >&2
  return 1
}

export_manifests() {
  local out="$EXP_DIR/socialnet-manifests.yaml"
  kubectl get deployments,services,hpa -n "$NAMESPACE" -o yaml > "$out"
  echo "Exported live manifests to $out"
}
