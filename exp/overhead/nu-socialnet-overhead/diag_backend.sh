#!/bin/bash
#
# Diagnose a backend that dies with "failed to start runtime" on a server node.
#
# Nu writes its auto-generated caladan conf with log_level 0 (command_line.cpp),
# which suppresses every log_err on the runtime-init path -- so a failed backend
# can only say "failed to start runtime". This script brings up iokerneld on ONE
# server node, runs the backend there in the foreground with an otherwise
# identical conf at log_level 6 (and optionally under strace), prints the
# output, and tears the node back down.
#
# Usage: ./diag_backend.sh [server_idx] [--strace]
#   server_idx defaults to the first entry of SERVER_IDXS (see common.sh).

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

IDX="${SERVER_IDXS[0]}"
USE_STRACE=0
for arg in "$@"; do
  case "$arg" in
    --strace) USE_STRACE=1 ;;
    [0-9]*)   IDX="$arg" ;;
    *) die "Unknown option: $arg" ;;
  esac
done

# Map the node index to its 0-based server slot k (caladan IP 18.18.1.(k+2)).
K=-1
for i in "${!SERVER_IDXS[@]}"; do
  [[ "${SERVER_IDXS[$i]}" == "$IDX" ]] && K="$i"
done
[[ "$K" -ge 0 ]] || die "idx$IDX is not in SERVER_IDXS (${SERVER_IDXS[*]})"
CIP="$(server_caladan_ip "$K")"

require_built
detect_network
mkdir -p "$LOG_DIR"

echo "=== Diagnosing the backend on idx$IDX (caladan $CIP) ==="

echo "--- resetting the node and starting iokerneld ---"
remote "$IDX" "$CALADAN_RESET_CMD" >/dev/null 2>&1 || true
remote_bg "$IDX" "sudo bash -c 'cd $NU_DIR && ./caladan/iokerneld ias dbg </dev/null >/tmp/iokerneld.log 2>&1'"
for _ in $(seq 1 20); do remote "$IDX" 'grep -q MAC /tmp/iokerneld.log' 2>/dev/null && break; sleep 1; done
remote "$IDX" 'pgrep -x iokerneld >/dev/null' 2>/dev/null || die "iokerneld failed on idx$IDX"

TRACER=""
if [[ "$USE_STRACE" -eq 1 ]]; then
  remote "$IDX" 'command -v strace >/dev/null' || die "strace not installed on idx$IDX"
  TRACER="strace -f -o /tmp/nu_diag.strace"
fi

echo "--- running the backend with a verbose conf (log_level 6) ---"
remote "$IDX" "kt=\$(( \$(nproc) / \$(ls -d /sys/devices/system/node/node[0-9]* | wc -l) - 2 ));
  { echo 'host_addr $CIP'; echo 'host_netmask 255.255.255.0'; echo 'host_gateway 18.18.1.1';
    echo 'host_mtu 9000'; echo \"runtime_kthreads \$kt\"; echo 'runtime_guaranteed_kthreads 0';
    echo 'runtime_spinning_kthreads 0'; echo 'runtime_priority be'; echo 'runtime_qdelay_us 10';
    echo 'enable_directpath 1'; echo 'log_level 6'; echo 'runtime_react_mem_pressure 1';
    echo 'runtime_react_cpu_pressure 1'; } > /tmp/nu_diag.conf;
  cd $SOCIALNET_DIR && sudo timeout 25 $TRACER ./build/src/main -l $LPID -f /tmp/nu_diag.conf" \
  > "$LOG_DIR/backend.diag.log" 2>&1
tail -30 "$LOG_DIR/backend.diag.log" | sed 's/^/  /'

if [[ "$USE_STRACE" -eq 1 ]]; then
  echo ""
  echo "--- failing syscalls from the strace (full trace: idx$IDX:/tmp/nu_diag.strace) ---"
  remote "$IDX" 'grep -nE "= -1 E|ENOMEM" /tmp/nu_diag.strace | tail -30' \
    | sed 's/^/  /' > "$LOG_DIR/backend.diag.strace-tail"
  cat "$LOG_DIR/backend.diag.strace-tail"
fi

echo "--- tearing the node back down ---"
remote "$IDX" "$CALADAN_RESET_CMD" >/dev/null 2>&1 || true
echo "Done. Full output: $LOG_DIR/backend.diag.log"
