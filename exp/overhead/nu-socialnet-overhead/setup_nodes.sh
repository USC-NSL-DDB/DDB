#!/bin/bash
#
# Prepare every node for a distributed Caladan/Nu run:
#   * hugepages + ksched module, jumbo frames, DSCP trust  (Nu's setup.sh)
#   * replicate the Nu tree to every server node at the same absolute path
#     (Nu ssh'es into `cd <path>`, and there is no shared filesystem here)
#
# No docker / nginx: seeding is native (build/init_graph over Thrift).
#
# Usage: ./setup_nodes.sh [--skip-sync]

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SKIP_SYNC=0
[[ "${1:-}" == "--skip-sync" ]] && SKIP_SYNC=1

detect_network
echo "=== Topology ==="
echo "  caladan NIC : $CALADAN_NIC   (DPDK binds this port)"
echo "  ssh network : ${SSH_PREFIX}x"
echo "  node0 (idx $INFRA_IDX, $(node_ip "$INFRA_IDX")): controller + client + init + DDB"
for k in $(seq 0 $((NUM_SERVERS - 1))); do
  idx="${SERVER_IDXS[$k]}"
  echo "  node$((k+1)) (idx $idx, $(node_ip "$idx")): Nu server, caladan $(server_caladan_ip "$k")$([[ "$idx" == "$MAIN_SERVER_IDX" ]] && echo '  [main -m]')"
done

echo ""
echo "=== Sanity: every node reachable with passwordless sudo ==="
for idx in "$INFRA_IDX" "${SERVER_IDXS[@]}"; do
  remote "$idx" 'sudo -n true' >/dev/null 2>&1 || die "idx$idx ($(node_ip "$idx")) unreachable or no passwordless sudo"
  echo "  idx$idx ok"
done

if [[ "$SKIP_SYNC" -eq 0 ]]; then
  echo ""
  echo "=== Replicating the Nu tree to every server node ==="
  for idx in "${SERVER_IDXS[@]}"; do
    ip="$(node_ip "$idx")"
    echo "  -> idx$idx ($ip)"
    remote "$idx" "sudo mkdir -p $NU_DIR && sudo chown -R \$(id -u):\$(id -g) $REPO_ROOT"
    rsync -az --delete \
      --exclude 'ddb/target' --exclude '.git' --exclude 'fwks/quicksand' \
      --exclude 'fwks/gotosocial' --exclude 'fwks/grpc' \
      "$NU_DIR/" "$ip:$NU_DIR/" 2>/dev/null || die "rsync to idx$idx failed"
  done
fi

echo ""
echo "=== Caladan machine setup (hugepages, ksched) on all caladan nodes ==="
for idx in "$INFRA_IDX" "${SERVER_IDXS[@]}"; do
  echo "  idx$idx"
  remote "$idx" "cd $NU_DIR && sudo ./caladan/scripts/setup_machine.sh >/dev/null 2>&1 || true"
  remote "$idx" "lsmod | grep -q ksched" || die "ksched not loaded on idx$idx. Build it: make -C $NU_DIR/caladan/ksched"
  remote "$idx" "sudo ifconfig $CALADAN_NIC mtu 9000 2>/dev/null; sudo mlnx_qos -i $CALADAN_NIC --trust dscp >/dev/null 2>&1 || true"
done

echo ""
echo "=== Setup complete ==="
echo "Next: ./run_benchmark.sh            (baseline)"
echo "      ./run_benchmark.sh --ddb      (DDB attached to all servers)"
