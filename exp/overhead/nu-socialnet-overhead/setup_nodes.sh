#!/bin/bash
#
# Prepare every node for a distributed Caladan/Nu run:
#   * hugepages + ksched module, jumbo frames, DSCP trust  (Nu's setup.sh)
#   * ship the Nu *runtime* subset (~64 MB: iokerneld + rdma libs + ksched +
#     the socialnet config) to every server node at the same absolute path --
#     remote nodes only run, never build, so the full 2.7 GB tree is not needed
#
# No docker / nginx: seeding is native (build/init_graph over Thrift).
#
# Usage: ./setup_nodes.sh [--skip-sync]

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SKIP_SYNC=0
[[ "${1:-}" == "--skip-sync" ]] && SKIP_SYNC=1

detect_network
mapfile -t ALL_IDXS < <(all_node_idxs)
echo "=== Topology ==="
echo "  caladan NIC : $CALADAN_NIC   (DPDK binds this port)"
echo "  ssh network : ${SSH_PREFIX}x"
echo "  idx $INFRA_IDX ($(node_ip "$INFRA_IDX")): controller + DDB + init_graph"
for n in $(seq 1 "$NUM_CLIENTS"); do
  idx="${CLIENT_NODES[$((n-1))]}"
  echo "  idx $idx ($(node_ip "$idx")): client$n (caladan 18.18.1.$((248+n)))"
done
for k in $(seq 0 $((NUM_SERVERS - 1))); do
  idx="${SERVER_IDXS[$k]}"
  echo "  idx $idx ($(node_ip "$idx")): Nu server, caladan $(server_caladan_ip "$k")$([[ "$idx" == "$MAIN_SERVER_IDX" ]] && echo '  [main -m]')"
done

echo ""
echo "=== Sanity: every node reachable with passwordless sudo ==="
for idx in "${ALL_IDXS[@]}"; do
  remote "$idx" 'sudo -n true' >/dev/null 2>&1 || die "idx$idx ($(node_ip "$idx")) unreachable or no passwordless sudo"
  # /mnt/local is often root-owned on a fresh node; claim it before we mkdir/rsync into it below.
  remote "$idx" 'sudo chown -R $(whoami): /mnt/local' || die "could not claim /mnt/local on idx$idx"
  echo "  idx$idx ok"
done

if [[ "$SKIP_SYNC" -eq 0 ]]; then
  echo ""
  echo "=== Shipping the Nu runtime bits to every non-local node ==="
  # A remote node never *builds* Nu -- it only runs iokerneld and the socialnet
  # backend (both scp'd/launched from here), so it needs a ~64 MB runtime subset,
  # not the full 2.7 GB tree. `-R` with the `/./` anchor recreates each path
  # under $NU_DIR on the receiver. What each entry is for:
  #   caladan/iokerneld                 - the iokernel binary (remote_bg on every node)
  #   caladan/rdma-core/build/lib       - iokerneld + main/client rpath into here (ldd)
  #   caladan/ksched/build/ksched.ko    - insmod'd by setup_machine.sh below
  #   caladan/scripts                   - setup_machine.sh + its siblings
  #   app/.../single_proclet/config     - main reads config/service-config.json (main.cpp)
  #   app/.../single_proclet/src, bench - RUNPATH anchors: main/client embed
  #       "<src|bench>/../../../../caladan/rdma-core/build/lib" as their RUNPATH,
  #       and path resolution needs every intermediate component to EXIST. If
  #       src/ (bench/) is missing, ld.so silently falls back to the SYSTEM
  #       libmlx5/libibverbs, and caladan directpath then fails its runtime
  #       init with a bare "failed to start runtime" (ret = -12).
  # The main/client binaries themselves are scp'd fresh by run_benchmark.sh, and
  # Nu generates its caladan .conf on the fly (runtime.cpp), so neither ships here.
  for idx in "${ALL_IDXS[@]}"; do
    [[ "$idx" == "$INFRA_IDX" ]] && continue     # infra node is where we run from
    ip="$(node_ip "$idx")"
    echo "  -> idx$idx ($ip)"
    remote "$idx" "sudo mkdir -p $NU_DIR && sudo chown -R \$(id -u):\$(id -g) $REPO_ROOT"
    rsync -azR \
      "$NU_DIR/./caladan/iokerneld" \
      "$NU_DIR/./caladan/rdma-core/build/lib" \
      "$NU_DIR/./caladan/ksched/build/ksched.ko" \
      "$NU_DIR/./caladan/scripts" \
      "$NU_DIR/./app/socialNetwork/single_proclet/config" \
      "$NU_DIR/./app/socialNetwork/single_proclet/src" \
      "$NU_DIR/./app/socialNetwork/single_proclet/bench" \
      "$ip:$NU_DIR/" 2>/dev/null || die "rsync to idx$idx failed"
    # Belt and braces: the libs the benchmark binaries MUST pick up over the
    # system rdma-core. Catch a broken RUNPATH here, not mid-benchmark.
    remote "$idx" "[[ -e $NU_DIR/app/socialNetwork/single_proclet/src ]]" \
      || die "idx$idx: RUNPATH anchor dir missing after rsync"
  done
fi

echo ""
echo "=== Caladan machine setup (hugepages, ksched) on all nodes ==="
for idx in "${ALL_IDXS[@]}"; do
  echo "  idx$idx"
  remote "$idx" "cd $NU_DIR && sudo ./caladan/scripts/setup_machine.sh >/dev/null 2>&1 || true"
  remote "$idx" "lsmod | grep -q ksched" || die "ksched not loaded on idx$idx. Build it: make -C $NU_DIR/caladan/ksched"
  remote "$idx" "sudo ifconfig $CALADAN_NIC mtu 9000 2>/dev/null; sudo mlnx_qos -i $CALADAN_NIC --trust dscp >/dev/null 2>&1 || true"
done

echo ""
echo "=== Setup complete ==="
echo "Next: ./run_benchmark.sh            (baseline)"
echo "      ./run_benchmark.sh --ddb      (DDB attached to all servers)"
