#!/bin/bash
#
# Run the Nu socialNetwork benchmark, optionally with DDB attached.
#
#   ./run_benchmark.sh                 # baseline, no debugger
#   ./run_benchmark.sh --ddb           # DDB attached to the backend
#   ./run_benchmark.sh --mops 0.5      # override the client's target load
#
# Topology (indices are Nu's ssh_ip indices):
#   node1 backend (Nu main server)   node2 controller
#   node3 nginx + docker stack       node4 client
#
# Results land in results/; per-process logs in logs/.

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

USE_DDB=0
MOPS="0.5"
LPID=1
# One Nu backend (node1). The nginx config hard-codes its caladan IP, so this
# experiment is single-backend by construction.
NUM_ENTRIES=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ddb)  USE_DDB=1; shift ;;
    --mops) MOPS="$2"; shift 2 ;;
    *) die "Unknown option: $1" ;;
  esac
done

require_built
mkdir -p "$LOG_DIR" "$RESULTS_DIR"

CALADAN_NIC="${CALADAN_NIC:-$(caladan_nic)}"; export CALADAN_NIC
SSH_PREFIX="$(ssh_prefix "$CALADAN_NIC")"; export SSH_PREFIX
[[ -n "$SSH_PREFIX" ]] || die "could not determine the ssh network"

TAG="mops${MOPS}$([[ $USE_DDB -eq 1 ]] && echo _ddb || echo _baseline)_$(date +%Y%m%d_%H%M%S)"
RESULT="$RESULTS_DIR/$TAG.txt"

cleanup_all() { "$EXP_DIR/stop_all.sh" >/dev/null 2>&1 || true; }
trap cleanup_all EXIT

echo "=== Cleaning up any previous run ==="
cleanup_all; sleep 2

echo ""
echo "=== Configuring the workload (${MOPS} Mops, ${NUM_ENTRIES} backend) ==="
# The client fans out to 18.18.1.{2..kNumEntries+1}; the backend creates that
# many service entries. If the two disagree the client connects to a node that
# does not exist and dies in TSocket::openConnection. Pin both, every run: the
# checked-in bench/client.cpp can be left at 7 by the nu_multi experiment.
cp "$NU_DIR/exp/social_net/nu/client.cpp" "$SOCIALNET_DIR/bench/client.cpp"
sed -i "s/constexpr static double kTargetMops.*/constexpr static double kTargetMops = $MOPS;/g" \
  "$SOCIALNET_DIR/bench/client.cpp"
sed -i "s/constexpr static uint32_t kNumEntries.*/constexpr static uint32_t kNumEntries = $NUM_ENTRIES;/g" \
  "$SOCIALNET_DIR/bench/client.cpp"
sed -i "s/constexpr uint32_t kNumEntries.*/constexpr uint32_t kNumEntries = $NUM_ENTRIES;/g" \
  "$SOCIALNET_DIR/src/main.cpp"
( cd "$SOCIALNET_DIR/build" && TMPDIR=/mnt/local/tmp make -j"$(nproc)" client main >/dev/null 2>&1 ) \
  || die "failed to rebuild client/main"
echo "  client kNumEntries=$(grep -m1 -oP 'kNumEntries = \K[0-9]+' "$SOCIALNET_DIR/bench/client.cpp")" \
     " main kNumEntries=$(grep -m1 -oP 'kNumEntries = \K[0-9]+' "$SOCIALNET_DIR/src/main.cpp")"

echo ""
echo "=== Distributing binaries ==="
for spec in "$BACKEND_IDX:build/src/main" "$CLIENT_IDX:build/bench/client"; do
  idx="${spec%%:*}"; rel="${spec#*:}"
  remote "$idx" "mkdir -p $SOCIALNET_DIR/$(dirname "$rel")"
  scp -q "$SOCIALNET_DIR/$rel" "$(node_ip "$idx"):$SOCIALNET_DIR/$rel" || die "scp $rel -> node$idx"
  echo "  node$idx <- $rel"
done

echo ""
echo "=== Bringing up the nginx / storage stack (node$NGINX_IDX) ==="
remote "$NGINX_IDX" "cd $SOCIALNET_DIR && ./down_nginx.sh >/dev/null 2>&1; ./up_nginx.sh" \
  > "$LOG_DIR/nginx.log" 2>&1 || die "nginx stack failed; see logs/nginx.log"
remote "$NGINX_IDX" "sudo ip addr add $NGINX_CALADAN_IP_AND_MASK dev $CALADAN_NIC 2>/dev/null || true"
echo "  up"

echo ""
echo "=== Starting iokerneld (debugger-aware) on caladan nodes ==="
# `ias dbg` = debugger-aware mode: caladan blocks its preemption signal while a
# debugger has the process stopped.
for i in $CTRL_IDX $BACKEND_IDX $CLIENT_IDX; do
  remote_bg "$i" "cd $NU_DIR && sudo ./caladan/iokerneld ias dbg </dev/null >/tmp/iokerneld.log 2>&1"
done
for i in $CTRL_IDX $BACKEND_IDX $CLIENT_IDX; do
  for _ in $(seq 1 20); do
    remote "$i" 'grep -q MAC /tmp/iokerneld.log' 2>/dev/null && break
    sleep 1
  done
  remote "$i" 'pgrep -x iokerneld >/dev/null' 2>/dev/null \
    || die "iokerneld failed on node$i; see /tmp/iokerneld.log there"
  echo "  node$i up"
done

echo ""
echo "=== Starting the Nu controller (node$CTRL_IDX) ==="
remote_bg "$CTRL_IDX" "cd $NU_DIR && sudo stdbuf -o0 ./bin/ctrl_main </dev/null >/tmp/ctrl_main.log 2>&1"
for _ in $(seq 1 20); do
  remote "$CTRL_IDX" 'pgrep -x ctrl_main >/dev/null' 2>/dev/null && break
  sleep 1
done
remote "$CTRL_IDX" 'pgrep -x ctrl_main >/dev/null' 2>/dev/null || die "ctrl_main failed to start"
echo "  up"
sleep 4

DDB_ARGS=""
if [[ "$USE_DDB" -eq 1 ]]; then
  echo ""
  echo "=== Starting DDB (node$BACKEND_IDX) ==="
  # DDB must be listening before the server reports itself, and its broker
  # writes the service-discovery config the server reads.
  "$EXP_DIR/start_ddb.sh" || die "failed to start DDB"
  DDB_ARGS="--ddb --ddb_node_ip $(node_ip "$BACKEND_IDX") --ddb_sd_config_path $DDB_SD_CONFIG"
fi

echo ""
echo "=== Starting the socialnet backend (node$BACKEND_IDX) ==="
BACKEND_CALADAN_IP="18.18.1.$((BACKEND_IDX + 1))"
remote_bg "$BACKEND_IDX" "cd $SOCIALNET_DIR && sudo stdbuf -o0 ./build/src/main \
  -m -l $LPID -i $BACKEND_CALADAN_IP $DDB_ARGS </dev/null >/tmp/backend.log 2>&1"

if [[ "$USE_DDB" -eq 1 ]]; then
  echo "  waiting for DDB to attach..."
  # The connector parks in sigwait; DDB attaches, sends SIG40, and the
  # connector's handler raises SIGTRAP. That trap is the "attached" signal.
  # GDB/MI orders record fields arbitrarily, so match one field, not a sequence.
  for i in $(seq 1 40); do
    grep -q 'signal-name="SIGTRAP"' "$LOG_DIR/ddb.log" 2>/dev/null && break
    sleep 2
  done
  grep -q 'signal-name="SIGTRAP"' "$LOG_DIR/ddb.log" 2>/dev/null \
    || die "DDB never attached; see logs/ddb.log"
  echo "  attached. Resuming the debuggee (-exec-continue)."
  # DDB stops the process after the SIG40 handshake; release it so it can serve.
  echo "-exec-continue" > "$LOG_DIR/ddb_in"
fi

echo "  waiting for the backend to serve..."
for i in $(seq 1 60); do
  remote "$BACKEND_IDX" "grep -q 'Starting the ThriftBackEndServer' /tmp/backend.log" 2>/dev/null && break
  sleep 2
done
remote "$BACKEND_IDX" "grep -q 'Starting the ThriftBackEndServer' /tmp/backend.log" 2>/dev/null \
  || die "backend never came up; check /tmp/backend.log on node$BACKEND_IDX"
echo "  backend up"

echo ""
echo "=== Seeding the social graph ==="
remote "$NGINX_IDX" "cd $SOCIALNET_DIR && python3 scripts/init_social_graph.py" \
  > "$LOG_DIR/init_graph.log" 2>&1 || die "graph init failed; see logs/init_graph.log"
sleep 5
echo "  seeded"

if [[ "$USE_DDB" -eq 1 ]]; then
  echo ""
  echo "=== Verifying DDB is still attached to the backend ==="
  # Running sidecar/session logs are not proof. Ask the kernel: TracerPid must
  # name a gdb, and the process must be running (S), not stopped (t).
  probe=$(remote "$BACKEND_IDX" 'p=$(pgrep -x main | head -1)
      sudo awk "/TracerPid/{print \$2}" /proc/$p/status
      sudo awk "/^State/{print \$2}" /proc/$p/status' 2>/dev/null)
  tracer=$(echo "$probe" | sed -n 1p); state=$(echo "$probe" | sed -n 2p)
  [[ -n "$tracer" && "$tracer" != "0" ]] || die "backend is NOT traced (TracerPid=$tracer)"
  [[ "$state" == "S" || "$state" == "R" ]] || die "backend is stopped (State=$state); it never resumed"
  echo "  TracerPid=$tracer State=$state  ->  attached and running"
fi

echo ""
echo "=== Running the client (node$CLIENT_IDX) ==="
# The client node only has the Nu tree, not this experiment dir; ship the conf.
scp -q "$EXP_DIR/conf/client" "$(node_ip "$CLIENT_IDX"):/tmp/nu_client.conf" \
  || die "could not copy the client conf to node$CLIENT_IDX"
remote "$CLIENT_IDX" "cd $SOCIALNET_DIR && sudo ./build/bench/client /tmp/nu_client.conf" \
  2>&1 | tee "$RESULT"

echo ""
echo "=== Done ==="
echo "Result: $RESULT"
[[ "$USE_DDB" -eq 1 ]] && echo "DDB log: $LOG_DIR/ddb.log"
