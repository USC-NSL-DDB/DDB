#!/bin/bash
#
# Measure the throughput of a distributed 3-node raft cluster under three
# debugger conditions. One invocation = one data point: it brings the cluster up,
# runs the load generator once, and tears everything down.
#
#   ./run_benchmark.sh --mode none    # no debugger attached  (the baseline)
#   ./run_benchmark.sh --mode ddb     # DDB attached to all 3 raft nodes
#   ./run_benchmark.sh --mode gdb     # bare gdb/MI attached to all 3 raft nodes
#
# The gdb mode drives gdb through the SAME interface DDB uses -- MI
# (--interpreter=mi3, mi-async on, same prerun commands, event stream over ssh).
# Comparing ddb against a console/batch gdb would flatter the baseline: the MI
# interpreter streams a notification per thread event where the console one
# stays silent, and this workload generates ~1M thread events per run. It does
# NOT reproduce DDB's broker/sigwait/SIG40 handshake -- that machinery exists so
# DDB can discover and attach to already-running processes; a baseline gdb that
# launches the node itself doesn't need it.
#
#   --clients N   concurrent client threads   (default 1024)
#   --reqs N      requests per client thread  (default 200)
#   --rounds N    benchmark rounds per run    (default 2)
#
# The default of 1024 client threads is not arbitrary -- it is this cluster's
# knee, where debugger overhead is most visible (~20% for gdb/MI and DDB alike;
# see README, "Choosing the load"). Raft only replicates on the leader's 80ms
# heartbeat, so at a low client count throughput is just num_clients/85ms and
# the servers idle: a debugger could cost 2x CPU and the number would not move.
# Past the knee the baseline is already queue-dominated and the relative
# overhead shrinks again (~13-17% at 2048).
#
# Layout (see common.sh):
#   node0 : tput_remote (load generator) + the ctrl server (+ DDB + broker)
#   node1-3 : one raft_node each, ids 1..3
#
# The three modes differ ONLY in whether a debugger is attached to the three
# raft_node processes. The binary, its flags, the load, and the client are
# identical -- so the difference in throughput is the debugger's overhead.

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

MODE="none"
CLIENTS=1024
REQS=200
ROUNDS=2
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)    MODE="$2";    shift 2 ;;
    --clients) CLIENTS="$2"; shift 2 ;;
    --reqs)    REQS="$2";    shift 2 ;;
    --rounds)  ROUNDS="$2";  shift 2 ;;
    *) die "Unknown option: $1 (expected --mode none|ddb|gdb)" ;;
  esac
done
case "$MODE" in
  none|ddb|gdb) ;;
  *) die "--mode must be one of: none, ddb, gdb (got '$MODE')" ;;
esac

require_built
mkdir -p "$LOG_DIR" "$RESULTS_DIR"

# One run at a time. Two concurrent invocations shoot each other down in ugly,
# asymmetric ways (each starts by killing every process the other depends on,
# and a finishing run's exit trap does it again) -- fail fast instead. The lock
# is held until this process exits, i.e. through the cleanup trap too.
exec 200>"$EXP_DIR/.run.lock"
flock -n 200 || die "another run_benchmark.sh is still active (or tearing down); wait for it or ./stop_all.sh"

TAG="c${CLIENTS}_r${REQS}_n${NUM_SERVERS}_${MODE}_$(date +%Y%m%d_%H%M%S)"
RESULT="$RESULTS_DIR/$TAG.txt"

cleanup_all() { "$EXP_DIR/stop_all.sh" >/dev/null 2>&1 || true; }
trap cleanup_all EXIT
echo "=== Cleaning up any previous run ==="; cleanup_all; sleep 2

echo ""
echo "=== Run: mode=$MODE, ${NUM_SERVERS} raft nodes, ${CLIENTS} client threads x ${REQS} reqs x ${ROUNDS} rounds ==="

# ─── The cluster config the load generator reads ─────────────────────────────
{
  echo '{'
  echo '  "nodes": ['
  for k in $(seq 0 $((NUM_SERVERS - 1))); do
    printf '    {"id": %s, "host": "%s", "tester_port": %s}%s\n' \
      "$(server_id "$k")" "${SERVER_IPS[$k]}" "$TESTER_PORT" \
      "$([[ $k -lt $((NUM_SERVERS - 1)) ]] && echo ,)"
  done
  echo '  ]'
  echo '}'
} > "$NODES_JSON"

# ─── DDB first: the raft nodes must find the broker the moment they start ────
if [[ "$MODE" == "ddb" ]]; then
  echo ""
  echo "=== Starting DDB on node0 ==="
  "$EXP_DIR/start_ddb.sh" || die "failed to start DDB"
fi


# ─── The load generator, BEFORE the raft nodes ───────────────────────────────
# raft_node's report_ready() is a fire-and-forget unary RPC with no retry: if the
# ctrl server is not already listening when a node starts, that node is never
# seen and tput_remote waits for it forever. So the client goes up first; it
# blocks on "Waiting for all 3 node(s) to report ready".
echo ""
echo "=== Starting the load generator on node0 (ctrl :$CTRL_PORT) ==="
CLIENT_LOG="$LOG_DIR/client.log"
rm -f "$CLIENT_LOG"
( cd "$RAFT_DIR" && stdbuf -o0 "$TPUT_BIN" \
    --config "$NODES_JSON" \
    --ctrl_addr "0.0.0.0:$CTRL_PORT" \
    --num_clients "$CLIENTS" \
    --num_reqs "$REQS" \
    --num_rounds "$ROUNDS" ) > "$CLIENT_LOG" 2>&1 &
CLIENT_PID=$!

for _ in $(seq 1 30); do
  grep -q 'Ctrl server listening' "$CLIENT_LOG" 2>/dev/null && break
  sleep 1
done
grep -q 'Ctrl server listening' "$CLIENT_LOG" 2>/dev/null \
  || die "ctrl server never came up; see $CLIENT_LOG"
echo "  up"

# ─── The raft nodes ──────────────────────────────────────────────────────────
mi_send() {  # $1 = ip, $2 = MI command line
  remote "$1" "printf '%s\n' '$2' > /tmp/gdb_mi_in"
}
mi_wait() {  # $1 = ip, $2 = ERE to wait for in that node's MI log
  local i
  for i in $(seq 1 30); do
    grep -qE "$2" "$LOG_DIR/gdb_mi.$1.log" 2>/dev/null && return 0
    sleep 1
  done
  die "$1: gdb/MI never produced '$2'; see $LOG_DIR/gdb_mi.$1.log"
}

launch_raft_node() {   # $1 = 0-based index
  local k="$1"
  local ip="${SERVER_IPS[$k]}" id peers
  id="$(server_id "$k")"
  peers="$(server_peers "$k")"

  local args="--id=$id --port=$RAFT_PORT --peers=$peers --node_tester_port=$TESTER_PORT"
  args+=" --enable_ctrl --ctrl_addr=$HEAD_IP:$CTRL_PORT --verbosity=0"

  case "$MODE" in
    none)
      remote_bg "$ip" "cd $RAFT_DIR && stdbuf -o0 $RAFT_NODE_BIN $args </dev/null >/tmp/raft_node.log 2>&1"
      ;;
    ddb)
      # --ddb_host_ip is THIS node's own address: it is what the connector reports
      # to the broker, and what DDB ssh'es back into to attach gdb. (raft-lab's
      # own exp/start_cluster.sh defaults it to the client's IP, which is only
      # right when everything runs on one machine.)
      #
      # wait_for_attach defaults to true, so the node parks in sigwait() before it
      # does anything -- including before report_ready() -- until DDB attaches and
      # we resume it below.
      remote_bg "$ip" "cd $RAFT_DIR && stdbuf -o0 $RAFT_NODE_BIN $args --ddb --ddb_host_ip=$ip </dev/null >/tmp/raft_node.log 2>&1"
      ;;
    gdb)
      # Launch raft_node under a gdb driven through the SAME interface DDB uses
      # -- MI (--interpreter=mi3, mi-async on, the same prerun commands), with
      # the MI event stream flowing back over the ssh channel like DDB's
      # sessions. A console/batch gdb would understate the baseline: MI emits a
      # notification per thread create/exit (this workload has ~1M of them per
      # run) where the console interpreter stays silent.
      #
      # Unlike DDB there is no broker / sigwait / SIG40 handshake -- that
      # machinery exists so DDB can discover and attach to an already-running
      # process; a baseline gdb doesn't need it. Launching under gdb debugs the
      # node from birth (so no attach race with the benchmark) and needs no
      # sudo, keeping raft_node an unprivileged child as in the other modes.
      #
      # The fifo holds the MI session's stdin open for the whole run. Its
      # holder opens it read-write (<>): a write-only open would block until
      # gdb reads, and that pending open keeps the ssh session's stdout busy --
      # sshd then never returns and this whole script hangs.
      remote "$ip" "rm -f /tmp/gdb_mi_in /tmp/gdb_mi_holder.pid && mkfifo /tmp/gdb_mi_in && { nohup sleep 86400 <> /tmp/gdb_mi_in >/dev/null 2>&1 & echo \$! > /tmp/gdb_mi_holder.pid; }"
      rm -f "$LOG_DIR/gdb_mi.$ip.log"
      # ssh -f keeps the channel open; gdb's MI stream flows back here.
      ssh -f -n -o BatchMode=yes -o StrictHostKeyChecking=no "$ip" \
        "cd $RAFT_DIR && gdb --interpreter=mi3 -q < /tmp/gdb_mi_in" > "$LOG_DIR/gdb_mi.$ip.log" 2>&1
      mi_send "$ip" '-gdb-set mi-async on'
      mi_send "$ip" '-enable-pretty-printing'
      mi_send "$ip" '-interpreter-exec console "handle SIGPIPE nostop noprint pass"'
      mi_send "$ip" "-file-exec-and-symbols $RAFT_NODE_BIN"
      mi_send "$ip" "-exec-arguments $args </dev/null >/tmp/raft_node.log 2>&1"
      mi_send "$ip" '-exec-run'
      mi_wait "$ip" '\^running'
      ;;
  esac
  echo "  node$((k + 1)) ($ip): raft id $id, peers $peers"
}

echo ""
echo "=== Starting $NUM_SERVERS raft nodes ($MODE) ==="
for k in $(seq 0 $((NUM_SERVERS - 1))); do launch_raft_node "$k"; done

# ─── Under DDB: wait for every node to attach, then resume each one ──────────
# Distinct sessions that have hit their post-attach SIGTRAP so far.
trapped_sids() {
  grep 'signal-name="SIGTRAP"' "$LOG_DIR/ddb.log" 2>/dev/null \
    | grep -oE 'session-id="[0-9]+"' | grep -oE '[0-9]+' | sort -un
}

if [[ "$MODE" == "ddb" ]]; then
  echo ""
  echo "=== Waiting for DDB to attach to all $NUM_SERVERS nodes ==="
  for _ in $(seq 1 60); do
    [[ "$(trapped_sids | wc -l)" -ge "$NUM_SERVERS" ]] && break
    sleep 2
  done
  n="$(trapped_sids | wc -l)"
  [[ "$n" -ge "$NUM_SERVERS" ]] || die "only $n/$NUM_SERVERS nodes attached; see $LOG_DIR/ddb.log"
  echo "  all $NUM_SERVERS attached"

  # Release them. Every node is parked in the connector's sigwait/SIGTRAP
  # handshake; until it is resumed it never reports ready, so the client is still
  # blocked and no load has been offered yet.
  while read -r sid; do
    echo "-exec-continue --session $sid" > "$LOG_DIR/ddb_in"
    echo "    resumed session $sid"
    sleep 1
  done < <(trapped_sids)
fi

# ─── The nodes are running: confirm the debugger really is attached ──────────
# TracerPid != 0 means something is ptrace-attached; State S/R means it is not
# sitting stopped. A mode that silently failed to attach would otherwise just
# reproduce the baseline number and look like "zero overhead".
echo ""
echo "=== Waiting for all $NUM_SERVERS nodes to report ready ==="
for _ in $(seq 1 60); do
  grep -q 'All nodes ready' "$CLIENT_LOG" 2>/dev/null && break
  kill -0 "$CLIENT_PID" 2>/dev/null || break
  sleep 2
done
grep -q 'All nodes ready' "$CLIENT_LOG" 2>/dev/null || {
  echo "  nodes did not all report ready; per-node logs:" >&2
  for ip in "${SERVER_IPS[@]}"; do
    echo "  --- $ip ---" >&2
    remote "$ip" 'sudo tail -5 /tmp/raft_node.log 2>/dev/null' >&2 2>/dev/null
    remote "$ip" "sudo cat /tmp/raft_node.log 2>/dev/null" > "$LOG_DIR/raft_node.$ip.log" 2>/dev/null
  done
  die "cluster never became ready (logs saved to $LOG_DIR/raft_node.*.log, client log $CLIENT_LOG)"
}
echo "  all $NUM_SERVERS ready"

if [[ "$MODE" != "none" ]]; then
  echo ""
  echo "=== Verifying the debugger is attached to all $NUM_SERVERS nodes ==="
  for ip in "${SERVER_IPS[@]}"; do
    probe=$(remote "$ip" 'p=$(pgrep -x raft_node|head -1); sudo awk "/TracerPid/{print \$2}" /proc/$p/status; sudo awk "/^State/{print \$2}" /proc/$p/status' 2>/dev/null)
    tp=$(echo "$probe" | sed -n 1p); st=$(echo "$probe" | sed -n 2p)
    [[ -n "$tp" && "$tp" != "0" ]] || die "$ip: raft_node is NOT traced (TracerPid=${tp:-none}) -- '$MODE' mode did not attach, the number would be meaningless"
    [[ "$st" == "S" || "$st" == "R" ]] || die "$ip: raft_node is stopped (State=$st), not running"
    echo "  $ip: TracerPid=$tp State=$st"
  done
elif [[ "$MODE" == "none" ]]; then
  # Symmetric check: the baseline must have NO debugger attached.
  for ip in "${SERVER_IPS[@]}"; do
    tp=$(remote "$ip" 'p=$(pgrep -x raft_node|head -1); sudo awk "/TracerPid/{print \$2}" /proc/$p/status' 2>/dev/null)
    [[ "$tp" == "0" ]] || die "$ip: baseline run has a debugger attached (TracerPid=$tp)"
  done
fi

# ─── Measure ─────────────────────────────────────────────────────────────────
echo ""
echo "=== Running the benchmark ($ROUNDS rounds) ==="
wait "$CLIENT_PID"; client_rc=$?
[[ "$client_rc" -eq 0 ]] || echo "  warning: the load generator exited $client_rc; see $CLIENT_LOG" >&2

# The debugger must have SURVIVED the measurement, or the number is a hybrid of
# debugged and undebugged execution. (This is not hypothetical: DDB once died
# mid-run when the root filesystem filled, and the orphaned gdbs kept the run
# alive to a plausible-looking but meaningless result.)
if [[ "$MODE" == "ddb" ]]; then
  kill -0 "$(cat "$LOG_DIR/ddb.pid" 2>/dev/null)" 2>/dev/null \
    || die "DDB died during the measurement; discard this run (see $LOG_DIR/ddb.log)"
fi
if [[ "$MODE" != "none" ]]; then
  for ip in "${SERVER_IPS[@]}"; do
    tp=$(remote "$ip" 'p=$(pgrep -x raft_node|head -1); [[ -n "$p" ]] && sudo awk "/TracerPid/{print \$2}" /proc/$p/status' 2>/dev/null)
    [[ -n "$tp" && "$tp" != "0" ]] \
      || die "$ip: raft_node no longer traced after the run (TracerPid=${tp:-gone}); discard this run"
  done
fi

grep -q '^AVG' "$CLIENT_LOG" || {
  tail -15 "$CLIENT_LOG" >&2
  die "the benchmark did not produce a result table; see $CLIENT_LOG"
}

# tput_remote prints one row per round plus an AVG row:
#   Round   latAvg(ms)  latP50(ms)  latP90(ms)  latP99(ms)  tput(Kops/s)
{
  echo "# mode=$MODE nodes=$NUM_SERVERS clients=$CLIENTS reqs=$REQS rounds=$ROUNDS"
  echo "# round latAvg_ms latP50_ms latP90_ms latP99_ms tput_kops"
  grep -E '^[0-9]+ ' "$CLIENT_LOG" | awk '{print $1, $2, $3, $4, $5, $6}'
  awk '/^AVG/ {print "avg_tput_kops", $6; print "avg_lat_ms", $2; print "p99_lat_ms", $5}' "$CLIENT_LOG"
} | tee "$RESULT"

echo ""
echo "=== Done ==="
echo "Result: $RESULT"
[[ "$MODE" == "ddb" ]] && echo "DDB log: $LOG_DIR/ddb.log"
echo "Compare modes at the same load:"
echo "  grep -H avg_tput_kops $RESULTS_DIR/c${CLIENTS}_r${REQS}_n${NUM_SERVERS}_*.txt"
