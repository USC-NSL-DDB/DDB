#!/bin/bash
#
# Tear everything down: DDB + gdb, the Nu processes on every node, iokerneld,
# the EMQX broker, and any stale Caladan shm / hugepage files.
# Safe to run anytime; run_benchmark.sh calls it on exit.

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SSH_PREFIX="${SSH_PREFIX:-$(ssh_prefix "$(caladan_nic 2>/dev/null || echo '')" 2>/dev/null || echo '')}"

# Local DDB session first, so gdb detaches before the debuggees are killed.
# Ask it to exit only if it is actually alive, and never block: opening a fifo
# whose reader died blocks forever in open(2) -- a `2>/dev/null` cannot save you
# -- and DDB can die mid-run (e.g. a full root filesystem).
if [[ -f "$LOG_DIR/ddb.pid" ]]; then
  DDB_PID="$(cat "$LOG_DIR/ddb.pid")"
  if kill -0 "$DDB_PID" 2>/dev/null; then
    timeout 3 bash -c "echo exit > '$LOG_DIR/ddb_in'" 2>/dev/null || true
    sleep 3
  fi
  kill -9 "$DDB_PID" 2>/dev/null || true
  rm -f "$LOG_DIR/ddb.pid"
fi
[[ -f "$LOG_DIR/ddb_holder.pid" ]] && { kill "$(cat "$LOG_DIR/ddb_holder.pid")" 2>/dev/null || true; rm -f "$LOG_DIR/ddb_holder.pid"; }
rm -f "$LOG_DIR/ddb_in"
pkill -9 -x ddb 2>/dev/null || true
docker rm -f emqx >/dev/null 2>&1 || true

# Reset every node (infra + clients + servers): kill processes, free shm + hugepages.
if [[ -n "$SSH_PREFIX" ]]; then
  while read -r idx; do
    remote "$idx" "$CALADAN_RESET_CMD" >/dev/null 2>&1 || true
  done < <(all_node_idxs)
fi
echo "Stopped."
