#!/bin/bash
#
# Prepare node1..node3 to run a raft_node:
#   * sanity: reachable, passwordless sudo, gdb installed
#   * replicate the raft-lab tree (binary + sources) to the same absolute path
#     -- there is no shared filesystem here, and gdb wants the sources next to
#     the debug info
#   * install libpaho-mqtt3c, the one shared library raft_node needs
#
# Re-run this after every ./build_all.sh: it is what ships the new binary.
#
# Usage: ./setup_nodes.sh [--skip-sync]

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SKIP_SYNC=0
[[ "${1:-}" == "--skip-sync" ]] && SKIP_SYNC=1

require_built

echo "=== Topology ==="
echo "  node0 ($HEAD_IP): tput_remote (load generator) + DDB + broker"
for k in $(seq 0 $((NUM_SERVERS - 1))); do
  echo "  node$((k + 1)) (${SERVER_IPS[$k]}): raft id $(server_id "$k"), raft=:$RAFT_PORT tester=:$TESTER_PORT"
done

echo ""
echo "=== Sanity: every server reachable, passwordless sudo, gdb present ==="
for ip in "${SERVER_IPS[@]}"; do
  remote "$ip" 'sudo -n true' >/dev/null 2>&1 || die "$ip unreachable or no passwordless sudo"
  remote "$ip" 'command -v gdb'  >/dev/null 2>&1 || die "gdb not installed on $ip (sudo apt-get install -y gdb)"
  echo "  $ip ok"
done

if [[ "$SKIP_SYNC" -eq 0 ]]; then
  echo ""
  echo "=== Replicating $RAFT_DIR to every server (binary + sources) ==="
  for ip in "${SERVER_IPS[@]}"; do
    echo "  -> $ip"
    remote "$ip" "sudo mkdir -p $RAFT_DIR && sudo chown -R \$(id -u):\$(id -g) $RAFT_DIR"
    # --delete so a stale raft_node from an older build can never be run by
    # mistake. .git is large and useless on the servers; libs/ is only needed to
    # compile, and we ship the finished binary.
    rsync -az --delete --exclude '.git' --exclude 'libs' \
      "$RAFT_DIR/" "$ip:$RAFT_DIR/" || die "rsync to $ip failed"
  done

  echo ""
  echo "=== libpaho-mqtt3c on every server ==="
  # raft_node links it unconditionally (the DDB connector's MQTT client), so the
  # binary will not even start without it -- in the baseline mode too.
  mapfile -t PAHO_LIBS < <(ldconfig -p | awk '/libpaho-mqtt3c(s)?\.so/{print $NF}' | sort -u)
  [[ ${#PAHO_LIBS[@]} -gt 0 ]] || die "libpaho-mqtt3c not installed on node0 either. Run ./build_all.sh"
  for ip in "${SERVER_IPS[@]}"; do
    if remote "$ip" 'ldconfig -p | grep -q "libpaho-mqtt3c\.so"'; then
      echo "  $ip: already present"
      continue
    fi
    for lib in "${PAHO_LIBS[@]}"; do
      # Copy through /tmp: scp cannot write /usr/local/lib without sudo.
      scp -q "$lib" "$ip:/tmp/$(basename "$lib")" || die "scp $lib -> $ip failed"
      remote "$ip" "sudo cp -a /tmp/$(basename "$lib") /usr/local/lib/ && rm -f /tmp/$(basename "$lib")"
    done
    remote "$ip" 'sudo ldconfig'
    echo "  $ip: installed ${#PAHO_LIBS[@]} lib(s)"
  done
fi

echo ""
echo "=== Verifying raft_node resolves all its libraries on every server ==="
for ip in "${SERVER_IPS[@]}"; do
  missing="$(remote "$ip" "ldd $RAFT_NODE_BIN 2>/dev/null | grep 'not found' || true")"
  [[ -z "$missing" ]] || die "$ip: raft_node has unresolved libraries:
$missing"
  echo "  $ip ok"
done

echo ""
echo "=== Setup complete ==="
echo "Next: ./run_benchmark.sh --mode none     (no debugger)"
echo "      ./run_benchmark.sh --mode ddb      (DDB attached to all 3 raft nodes)"
echo "      ./run_benchmark.sh --mode gdb      (plain gdb attached to all 3)"
