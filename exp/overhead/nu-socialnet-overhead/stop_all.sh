#!/bin/bash
#
# Tear everything down: DDB + gdb, the Nu processes, iokerneld, and the EMQX
# broker. Safe to run at any time; run_benchmark.sh calls it on exit.

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

CALADAN_NIC="${CALADAN_NIC:-$(caladan_nic 2>/dev/null || echo '')}"
SSH_PREFIX="${SSH_PREFIX:-$(ssh_prefix "$CALADAN_NIC" 2>/dev/null || echo '')}"

# Local DDB session first, so gdb detaches before we kill the debuggees.
if [[ -f "$LOG_DIR/ddb.pid" ]]; then
  echo "exit" > "$LOG_DIR/ddb_in" 2>/dev/null || true
  sleep 3
  kill -9 "$(cat "$LOG_DIR/ddb.pid")" 2>/dev/null || true
  rm -f "$LOG_DIR/ddb.pid"
fi
[[ -f "$LOG_DIR/ddb_holder.pid" ]] && { kill "$(cat "$LOG_DIR/ddb_holder.pid")" 2>/dev/null || true; rm -f "$LOG_DIR/ddb_holder.pid"; }
rm -f "$LOG_DIR/ddb_in"
pkill -9 -x ddb 2>/dev/null || true
docker rm -f emqx >/dev/null 2>&1 || true

# A SIGKILLed iokerneld (or a Nu process still attached) leaves its SysV shm
# segments and DPDK hugepage files behind, and the next iokerneld dies with
# "Shared memory region is already mapped". Clear both after killing.
CALADAN_RESET='sudo pkill -9 gdb; sudo pkill -9 -x main; sudo pkill -9 -x client;
  sudo pkill -9 ctrl_main; sudo pkill -9 ctrl_proxy; sudo pkill -9 iokerneld; sleep 1;
  for id in $(ipcs -m | awk "\$6 ~ /^[0-9]+$/ {print \$2}"); do sudo ipcrm -m "$id" 2>/dev/null; done;
  sudo rm -f /dev/hugepages/rtemap_* 2>/dev/null'

if [[ -n "$SSH_PREFIX" ]]; then
  for i in $BACKEND_IDX $CTRL_IDX $CLIENT_IDX; do
    remote "$i" "$CALADAN_RESET" >/dev/null 2>&1 || true
  done
  # nginx node: drop the caladan-side address; leave the containers up (slow to
  # rebuild). Use ./stop_all.sh --full to tear the storage stack down too.
  if [[ "${1:-}" == "--full" ]]; then
    remote "$NGINX_IDX" "cd $SOCIALNET_DIR && ./down_nginx.sh" >/dev/null 2>&1 || true
  fi
  [[ -n "$CALADAN_NIC" ]] && remote "$NGINX_IDX" \
    "sudo ip addr delete $NGINX_CALADAN_IP_AND_MASK dev $CALADAN_NIC" >/dev/null 2>&1 || true
fi

echo "Stopped."
