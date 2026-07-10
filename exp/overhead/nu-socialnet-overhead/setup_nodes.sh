#!/bin/bash
#
# Prepare every node for a Caladan/Nu run:
#   * hugepages, ksched module, jumbo frames, DSCP trust  (Nu's setup.sh)
#   * a copy of the repo at the same absolute path (Nu's shared.sh ssh'es into
#     `cd $(pwd)`, and there is no shared filesystem here)
#   * docker on the nginx node, with its data-root on the big disk
#
# Usage: ./setup_nodes.sh [--skip-sync]

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SKIP_SYNC=0
[[ "${1:-}" == "--skip-sync" ]] && SKIP_SYNC=1

echo "=== Detecting NICs ==="
CALADAN_NIC="$(caladan_nic)" || die "could not determine caladan NIC (is caladan built?)"
export CALADAN_NIC
SSH_PREFIX="$(ssh_prefix "$CALADAN_NIC")"
export SSH_PREFIX
[[ -n "$SSH_PREFIX" ]] || die "could not find the non-caladan 10.10.x network"
echo "  caladan NIC : $CALADAN_NIC   (DPDK binds this port)"
echo "  ssh network : ${SSH_PREFIX}x"
for i in $BACKEND_IDX $CTRL_IDX $NGINX_IDX $CLIENT_IDX; do
  echo "  node$i -> $(node_ip "$i")"
done

echo ""
echo "=== Sanity: every node reachable ==="
for i in $BACKEND_IDX $CTRL_IDX $NGINX_IDX $CLIENT_IDX; do
  remote "$i" 'sudo -n true' >/dev/null 2>&1 \
    || die "node$i ($(node_ip "$i")) unreachable or lacks passwordless sudo"
  echo "  node$i ok"
done

if [[ "$SKIP_SYNC" -eq 0 ]]; then
  echo ""
  echo "=== Replicating the repo to every other node (same absolute path) ==="
  for i in $CTRL_IDX $NGINX_IDX $CLIENT_IDX; do
    ip="$(node_ip "$i")"
    echo "  -> node$i ($ip)"
    # /mnt/local is root-owned on a fresh node; take ownership once.
    remote "$i" "sudo mkdir -p $NU_DIR && sudo chown -R \$(id -u):\$(id -g) $REPO_ROOT"
    # Binaries and the app tree; skip the giant build dirs we don't need remotely.
    rsync -az --delete \
      --exclude 'ddb/target' --exclude '.git' --exclude 'fwks/quicksand' \
      --exclude 'fwks/gotosocial' --exclude 'fwks/grpc' \
      "$REPO_ROOT/fwks/Nu/" "$ip:$NU_DIR/" 2>/dev/null || die "rsync to node$i failed"
  done
fi

echo ""
echo "=== Caladan machine setup (hugepages, ksched) on caladan nodes ==="
# The nginx node deliberately does NOT run iokerneld: DPDK would seize its NIC.
for i in $BACKEND_IDX $CTRL_IDX $CLIENT_IDX; do
  echo "  node$i"
  remote "$i" "cd $NU_DIR && sudo ./caladan/scripts/setup_machine.sh >/dev/null 2>&1 || true"
  remote "$i" "lsmod | grep -q ksched" \
    || die "ksched not loaded on node$i. Build it: make -C $NU_DIR/caladan/ksched"
  remote "$i" "sudo ifconfig $CALADAN_NIC mtu 9000; sudo mlnx_qos -i $CALADAN_NIC --trust dscp >/dev/null 2>&1 || true"
done

echo ""
echo "=== Graph-seeding deps on the nginx node (node$NGINX_IDX) ==="
# scripts/init_social_graph.py drives the HTTP API to register users/follows.
remote "$NGINX_IDX" 'python3 -c "import aiohttp" 2>/dev/null || {
    sudo apt-get update -qq && sudo apt-get install -y -qq python3-pip >/dev/null
    pip3 install --user aiohttp >/dev/null 2>&1 || pip3 install --user --break-system-packages aiohttp
  }
  python3 -c "import aiohttp; print(\"  aiohttp ok\")"'

echo ""
echo "=== Docker on the nginx node (node$NGINX_IDX) ==="
# / is only 16G on these images and the DeathStarBench stack needs several GB.
remote "$NGINX_IDX" 'sudo mkdir -p /mnt/local/docker
  if ! grep -qs "/mnt/local/docker" /etc/docker/daemon.json 2>/dev/null; then
    sudo mkdir -p /etc/docker
    echo "{\"data-root\": \"/mnt/local/docker\"}" | sudo tee /etc/docker/daemon.json >/dev/null
    sudo systemctl restart docker
  fi
  docker info 2>/dev/null | grep "Docker Root"'
echo ""

echo "=== Setup complete ==="
echo "Next: ./run_benchmark.sh            (baseline)"
echo "      ./run_benchmark.sh --ddb      (with DDB attached)"
