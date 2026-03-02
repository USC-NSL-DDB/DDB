#!/bin/bash
#
# Run the socialnet throughput benchmark with configurable parameters.
#
# Usage:
#   ./run_benchmark.sh                          # defaults: 10 threads, 0.00005 target MOPS, 120s
#   ./run_benchmark.sh --target-mops 0.0001     # higher load
#   ./run_benchmark.sh --target-mops 0.0005 --threads 20 --duration 60
#   ./run_benchmark.sh --sweep 0.00005,0.0001,0.0005   # sweep multiple MOPS values
#
# Results are saved to results/ directory.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOCIALNET_DIR="$SCRIPT_DIR/../../fwks/socialnetwork"
CLIENT_BIN="$SOCIALNET_DIR/src/client/client.out"
RESULTS_DIR="$SCRIPT_DIR/results"

# Defaults
TARGET_MOPS="0.00005"
THREADS="10"
DURATION="120"
WARMUP="4"
SWEEP=""
ADDR=""

# Parse arguments
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target-mops) TARGET_MOPS="$2"; shift 2 ;;
    --threads)     THREADS="$2";     shift 2 ;;
    --duration)    DURATION="$2";    shift 2 ;;
    --warmup)      WARMUP="$2";      shift 2 ;;
    --addr)        ADDR="$2";        shift 2 ;;
    --sweep)       SWEEP="$2";       shift 2 ;;
    *)             echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

# Auto-detect endpoint
if [[ -z "$ADDR" ]]; then
  master_ip="10.10.1.1"
  svc_name=$(kubectl get svc -o name 2>/dev/null | grep apilistener | head -1)
  if [[ -z "$svc_name" ]]; then
    echo "Error: no apilistener service found. Run setup_experiment.sh first." >&2
    exit 1
  fi
  node_port=$(kubectl get "$svc_name" -o jsonpath='{.spec.ports[0].nodePort}')
  ADDR="http://${master_ip}:${node_port}"
fi

if [[ ! -f "$CLIENT_BIN" ]]; then
  echo "Error: $CLIENT_BIN not found. Run setup_experiment.sh first." >&2
  exit 1
fi

mkdir -p "$RESULTS_DIR"

run_single() {
  local mops="$1"
  local timestamp
  timestamp=$(date +%Y%m%d_%H%M%S)
  local tag="mops${mops}_t${THREADS}_d${DURATION}_${timestamp}"
  local result_file="$RESULTS_DIR/${tag}.txt"
  local ts_file="$RESULTS_DIR/${tag}_timeseries.txt"

  echo "======================================"
  echo "Benchmark: target_mops=$mops threads=$THREADS duration=${DURATION}s"
  echo "Endpoint:  $ADDR"
  echo "Output:    $result_file"
  echo "======================================"

  "$CLIENT_BIN" \
    -addr "$ADDR" \
    -target-mops "$mops" \
    -threads "$THREADS" \
    -duration "$DURATION" \
    -warmup "$WARMUP" \
    -output "$result_file" \
    -timeseries "$ts_file" \
    2>&1 | tee -a "$result_file"

  echo ""
  echo "Results saved to: $result_file"
  echo "Timeseries saved to: $ts_file"
  echo ""
}

if [[ -n "$SWEEP" ]]; then
  IFS=',' read -ra MOPS_LIST <<< "$SWEEP"
  echo "Sweeping ${#MOPS_LIST[@]} target MOPS values: ${MOPS_LIST[*]}"
  echo ""
  for mops in "${MOPS_LIST[@]}"; do
    run_single "$mops"
    # drain ports between runs
    sleep 10
  done
  echo "=== Sweep complete. Results in $RESULTS_DIR ==="
else
  run_single "$TARGET_MOPS"
fi
