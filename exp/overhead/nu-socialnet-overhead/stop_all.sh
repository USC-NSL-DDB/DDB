#!/bin/bash
#
# Tear everything down: DDB + gdb, the Nu processes on every node, iokerneld,
# the EMQX broker, and any stale Caladan shm / hugepage files.
# Safe to run anytime; run_benchmark.sh calls it on exit.

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SSH_PREFIX="${SSH_PREFIX:-$(ssh_prefix "$(caladan_nic 2>/dev/null || echo '')" 2>/dev/null || echo '')}"

# Local DDB session first, so gdb detaches before the debuggees are killed.
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

# Reset every node (infra + servers): kill processes, free shm + hugepages.
if [[ -n "$SSH_PREFIX" ]]; then
  for idx in "$INFRA_IDX" "${SERVER_IDXS[@]}"; do
    remote "$idx" "$CALADAN_RESET_CMD" >/dev/null 2>&1 || true
  done
fi
echo "Stopped."
