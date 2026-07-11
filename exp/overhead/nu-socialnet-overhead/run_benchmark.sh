#!/bin/bash
#
# Run the distributed Nu socialNetwork benchmark, optionally with DDB attached
# to every backend server.
#
#   ./run_benchmark.sh                 # baseline, no debugger
#   ./run_benchmark.sh --ddb           # DDB attached to all 4 servers
#   ./run_benchmark.sh --mops 2.0      # override target load (Mops, total)
#
# Layout (see common.sh):
#   node0 : controller + init_graph + client  (+ DDB + EMQX broker with --ddb)
#   node1-4 : Nu servers, caladan 18.18.1.2-5, one ServiceEntry proclet each

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

USE_DDB=0
MOPS="1.0"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ddb)  USE_DDB=1; shift ;;
    --mops) MOPS="$2"; shift 2 ;;
    *) die "Unknown option: $1" ;;
  esac
done

require_built
detect_network
mkdir -p "$LOG_DIR" "$RESULTS_DIR"

TAG="mops${MOPS}_n${NUM_SERVERS}$([[ $USE_DDB -eq 1 ]] && echo _ddb || echo _baseline)_$(date +%Y%m%d_%H%M%S)"
RESULT="$RESULTS_DIR/$TAG.txt"

cleanup_all() { "$EXP_DIR/stop_all.sh" >/dev/null 2>&1 || true; }
trap cleanup_all EXIT
echo "=== Cleaning up any previous run ==="; cleanup_all; sleep 2

# ─── Build the workload with the right server count + load ───────────────────
echo ""
echo "=== Building workload: ${NUM_SERVERS} entries, ${MOPS} Mops ==="
# Use Nu's DISTRIBUTED client (nu_multi): perf.run_multi_clients across
# kClientAddrs (18.18.1.249..251) with a TCP barrier, each offering
# kTargetMops/N. kTargetMops here is the TOTAL target; the client divides it.
# client fans out to entries 18.18.1.2..; main creates kNumEntries ServiceEntry
# proclets that the controller spreads one-per-server. The two counts must agree.
cp "$NU_DIR/exp/social_net/nu_multi/client.cpp" "$SOCIALNET_DIR/bench/client.cpp"
sed -i "s/constexpr static double kTargetMops.*/constexpr static double kTargetMops = $MOPS;/g" \
  "$SOCIALNET_DIR/bench/client.cpp"
sed -i "s/constexpr static uint32_t kNumEntries.*/constexpr static uint32_t kNumEntries = $NUM_SERVERS;/g" \
  "$SOCIALNET_DIR/bench/client.cpp"
sed -i "s/constexpr uint32_t kNumEntries.*/constexpr uint32_t kNumEntries = $NUM_SERVERS;/g" \
  "$SOCIALNET_DIR/src/main.cpp"
( cd "$SOCIALNET_DIR/build" && TMPDIR=/mnt/local/tmp make -j"$(nproc)" main client >/dev/null 2>&1 ) \
  || die "failed to rebuild main/client"
echo "  main kNumEntries=$(grep -m1 -oP 'kNumEntries = \K[0-9]+' "$SOCIALNET_DIR/src/main.cpp")," \
     "client kNumEntries=$(grep -m1 -oP 'kNumEntries = \K[0-9]+' "$SOCIALNET_DIR/bench/client.cpp")"

mapfile -t ALL_IDXS < <(all_node_idxs)

# ─── Distribute the freshly-built binaries ───────────────────────────────────
echo ""
echo "=== Distributing binaries ==="
for idx in "${SERVER_IDXS[@]}"; do
  remote "$idx" "mkdir -p $SOCIALNET_DIR/build/src"
  scp -q "$SOCIALNET_DIR/build/src/main" "$(node_ip "$idx"):$SOCIALNET_DIR/build/src/main" || die "scp main -> idx$idx"
  echo "  idx$idx <- main"
done
# The freshly-rebuilt client goes to every remote client node + its conf.
for n in $(seq 1 "$NUM_CLIENTS"); do
  idx="${CLIENT_NODES[$((n-1))]}"
  [[ "$idx" == "$INFRA_IDX" ]] && continue     # client on the local node
  remote "$idx" "mkdir -p $SOCIALNET_DIR/build/bench"
  scp -q "$SOCIALNET_DIR/build/bench/client" "$(node_ip "$idx"):$SOCIALNET_DIR/build/bench/client" || die "scp client -> idx$idx"
  scp -q "$EXP_DIR/conf/client$n" "$(node_ip "$idx"):/tmp/nu_client$n.conf" || die "scp conf -> idx$idx"
  echo "  idx$idx <- client (client$n)"
done

# ─── iokerneld everywhere ────────────────────────────────────────────────────
echo ""
echo "=== Starting iokerneld (debugger-aware) on all nodes ==="
for idx in "${ALL_IDXS[@]}"; do
  # Redirect under sudo so root owns the log (a prior root-owned log would
  # otherwise make the user-shell redirect fail with EACCES).
  remote_bg "$idx" "sudo bash -c 'cd $NU_DIR && ./caladan/iokerneld ias dbg </dev/null >/tmp/iokerneld.log 2>&1'"
done
for idx in "${ALL_IDXS[@]}"; do
  for _ in $(seq 1 20); do remote "$idx" 'grep -q MAC /tmp/iokerneld.log' 2>/dev/null && break; sleep 1; done
  remote "$idx" 'pgrep -x iokerneld >/dev/null' 2>/dev/null || die "iokerneld failed on idx$idx"
done
echo "  up on idx ${ALL_IDXS[*]}"

# ─── Controller on node0 ─────────────────────────────────────────────────────
echo ""
echo "=== Starting the Nu controller on node0 (caladan $CTRL_CALADAN_IP) ==="
remote_bg "$INFRA_IDX" "sudo bash -c 'cd $NU_DIR && stdbuf -o0 ./bin/ctrl_main </dev/null >/tmp/ctrl_main.log 2>&1'"
for _ in $(seq 1 20); do remote "$INFRA_IDX" 'pgrep -x ctrl_main >/dev/null' 2>/dev/null && break; sleep 1; done
remote "$INFRA_IDX" 'pgrep -x ctrl_main >/dev/null' 2>/dev/null || die "ctrl_main failed"
echo "  up"; sleep 4

# ─── DDB (before the servers report themselves) ──────────────────────────────
DDB_COMMON_ARGS=""
if [[ "$USE_DDB" -eq 1 ]]; then
  echo ""
  echo "=== Starting DDB on node0 ==="
  "$EXP_DIR/start_ddb.sh" || die "failed to start DDB"
  DDB_COMMON_ARGS="--ddb --ddb_sd_config_path $DDB_SD_CONFIG"
fi

# ─── Servers: non-main first, then the main server ───────────────────────────
launch_server() {   # $1 = server index, $2 = 0-based k, $3 = "-m" or ""
  local idx="$1" k="$2" main_flag="$3"
  local cip; cip="$(server_caladan_ip "$k")"
  local ddb=""
  [[ "$USE_DDB" -eq 1 ]] && ddb="$DDB_COMMON_ARGS --ddb_node_ip $(node_ip "$idx")"
  # main's RUNPATH resolves the caladan libs, so no LD_LIBRARY_PATH needed.
  remote_bg "$idx" "sudo bash -c 'cd $SOCIALNET_DIR && stdbuf -o0 ./build/src/main $main_flag -l $LPID -i $cip $ddb </dev/null >/tmp/backend.log 2>&1'"
  echo "  idx$idx  caladan=$cip  ${main_flag:-(plain)}"
}

# Distinct sessions that have hit their post-attach SIGTRAP so far.
trapped_sids() {
  grep 'signal-name="SIGTRAP"' "$LOG_DIR/ddb.log" 2>/dev/null \
    | grep -oE 'session-id="[0-9]+"' | grep -oE '[0-9]+' | sort -un
}

# Wait until >= $1 servers have attached, then resume any not-yet-resumed
# session, one at a time with a pause, so each finishes its Nu runtime init and
# controller registration before the next. Resuming servers in lockstep makes
# them race distributed startup and segfaults one (NULL get_runtime() in the RPC
# archive pool).
RESUMED_SIDS=" "
ddb_resume_upto() {   # $1 = expected attached count, $2 = label
  local want="$1" label="$2" n sid
  echo "  [ddb] waiting for $want server(s) to attach ($label)..."
  for _ in $(seq 1 60); do
    [[ "$(trapped_sids | wc -l)" -ge "$want" ]] && break
    sleep 2
  done
  n="$(trapped_sids | wc -l)"
  [[ "$n" -ge "$want" ]] || die "only $n/$want servers attached ($label); see logs/ddb.log"
  while read -r sid; do
    [[ "$RESUMED_SIDS" == *" $sid "* ]] && continue
    echo "-exec-continue --session $sid" > "$LOG_DIR/ddb_in"
    RESUMED_SIDS+="$sid "
    echo "    resumed session $sid"
    sleep 6
  done < <(trapped_sids)
}

echo ""
echo "=== Starting the ${NUM_SERVERS} Nu servers ==="
# Plain (non-main) servers first. Under DDB they are attached, resumed, and
# registered with the controller BEFORE the main server is even started -- the
# main server's DoWork creates and places proclets across the cluster, so it
# must run last, against an already-registered set of servers.
for k in $(seq 0 $((NUM_SERVERS - 1))); do
  idx="${SERVER_IDXS[$k]}"
  [[ "$idx" == "$MAIN_SERVER_IDX" ]] && continue
  launch_server "$idx" "$k" ""
done
[[ "$USE_DDB" -eq 1 ]] && ddb_resume_upto "$((NUM_SERVERS - 1))" "plain servers"

# Main server (-m) last: boots the app once the others are up and registered.
launch_server "$MAIN_SERVER_IDX" "$((NUM_SERVERS - 1))" "-m"
[[ "$USE_DDB" -eq 1 ]] && ddb_resume_upto "$NUM_SERVERS" "main server"

# ─── Wait for the app to serve ───────────────────────────────────────────────
echo ""
echo "=== Waiting for the backend to serve ==="
served=0
for _ in $(seq 1 90); do
  remote "$MAIN_SERVER_IDX" "grep -q 'Starting the ThriftBackEndServer' /tmp/backend.log" 2>/dev/null && { served=1; break; }
  sleep 2
done
if [[ "$served" -eq 0 ]]; then
  echo "  backend did not serve; saving per-server logs to $LOG_DIR/ and dmesg:" >&2
  for idx in "${SERVER_IDXS[@]}"; do
    p=$(remote "$idx" 'pgrep -x main | head -1' 2>/dev/null)
    st=$(remote "$idx" "[[ -n '$p' ]] && sudo awk '/^State/{print \$2}' /proc/$p/status 2>/dev/null || echo DEAD" 2>/dev/null)
    echo "  --- idx$idx  pid=${p:-none}  State=${st:-?} ---" >&2
    remote "$idx" 'sudo cat /tmp/backend.log 2>/dev/null' > "$LOG_DIR/backend.idx$idx.log" 2>/dev/null
    tail -5 "$LOG_DIR/backend.idx$idx.log" 2>/dev/null | sed 's/^/      /' >&2
    # if the process is gone, dmesg often shows a segfault / OOM kill
    [[ "$st" == "DEAD" ]] && remote "$idx" 'sudo dmesg | tail -5' 2>/dev/null | grep -iE 'segfault|killed|oom|main' | sed 's/^/      dmesg: /' >&2
  done
  die "backend never came up (logs saved in $LOG_DIR/backend.idx*.log)"
fi
echo "  up"; sleep 3

# ─── Seed the graph natively (Thrift, no nginx) ──────────────────────────────
echo ""
echo "=== Seeding the social graph (init_graph -> $INIT_ENTRY_IP) ==="
( cd "$SOCIALNET_DIR" && sudo ./build/init_graph/init_graph "$EXP_DIR/conf/init" ) \
  > "$LOG_DIR/init_graph.log" 2>&1 || die "graph init failed; see logs/init_graph.log"
tail -1 "$LOG_DIR/init_graph.log" | sed 's/^/  /'
sleep 3

# ─── Verify DDB is still attached to every server ────────────────────────────
if [[ "$USE_DDB" -eq 1 ]]; then
  echo ""
  echo "=== Verifying DDB is attached to all $NUM_SERVERS servers ==="
  for idx in "${SERVER_IDXS[@]}"; do
    probe=$(remote "$idx" 'p=$(pgrep -x main|head -1); sudo awk "/TracerPid/{print \$2}" /proc/$p/status; sudo awk "/^State/{print \$2}" /proc/$p/status' 2>/dev/null)
    tp=$(echo "$probe" | sed -n 1p); st=$(echo "$probe" | sed -n 2p)
    [[ -n "$tp" && "$tp" != "0" ]] || die "idx$idx backend NOT traced (TracerPid=$tp)"
    [[ "$st" == "S" || "$st" == "R" ]] || die "idx$idx backend stopped (State=$st)"
    echo "  idx$idx: TracerPid=$tp State=$st"
  done
fi

# ─── Run the clients (spread across the client nodes) ────────────────────────
echo ""
echo "=== Running $NUM_CLIENTS clients (nu_multi, barrier-synced across nodes) ==="
# Each client offers kTargetMops/N and they rendezvous at a TCP barrier (sink =
# first kClientAddr), so all must run concurrently. Each client blocks until the
# benchmark finishes and prints its result to stdout, which we capture locally.
# ssh (no -f) is fine here: the client exits on its own, so the channel closes.
client_pids=()
for n in $(seq 1 "$NUM_CLIENTS"); do
  idx="${CLIENT_NODES[$((n-1))]}"
  if [[ "$idx" == "$INFRA_IDX" ]]; then
    ( cd "$LOG_DIR" && sudo "$SOCIALNET_DIR/build/bench/client" "$EXP_DIR/conf/client$n" ) \
      > "$LOG_DIR/client.$n.log" 2>&1 &
  else
    ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no "$(node_ip "$idx")" \
      "cd /tmp && sudo $SOCIALNET_DIR/build/bench/client /tmp/nu_client$n.conf" \
      > "$LOG_DIR/client.$n.log" 2>&1 &
  fi
  client_pids+=($!)
  echo "  client$n on idx$idx (caladan 18.18.1.$((248+n)))"
done
fail=0
for pid in "${client_pids[@]}"; do wait "$pid" || fail=1; done
[[ "$fail" -eq 0 ]] || echo "  warning: a client exited non-zero; see logs/client.*.log" >&2

# Total system throughput = sum of the per-client real_mops (each measured its
# own share over the barrier-synced window).
echo ""
{
  echo "# per-client (real_mops avg 50th 90th 95th 99th 99.9th):"
  total=0
  for n in $(seq 1 "$NUM_CLIENTS"); do
    line=$(grep -E '^[0-9]+\.[0-9]+ ' "$LOG_DIR/client.$n.log" | tail -1)
    printf "client%s %s\n" "$n" "${line:-MISSING}"
    m=$(awk '{print $1}' <<<"$line")
    [[ -n "$m" ]] && total=$(awk -v a="$total" -v b="$m" 'BEGIN{print a+b}')
  done
  echo "aggregate_real_mops $total"
} | tee "$RESULT"

echo ""
echo "=== Done ==="
echo "Result: $RESULT   (aggregate throughput = sum of $NUM_CLIENTS clients)"
[[ "$USE_DDB" -eq 1 ]] && echo "DDB log: $LOG_DIR/ddb.log"
