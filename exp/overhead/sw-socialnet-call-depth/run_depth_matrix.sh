#!/usr/bin/env bash
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck disable=SC1091
source "$HERE/common.sh"

ADDR=${ADDR:-$(detect_endpoint)}
OUT=${OUT:-$RESULTS_ROOT/call-depth-$(timestamp)}
WARMUP=${WARMUP:-3}
REPETITIONS=${REPETITIONS:-10}
EXPECTED_SESSIONS=${EXPECTED_SESSIONS:-$EXPECTED_PROCESSES}
DDB=${DDB:-$DDB_BIN}
CONFIG=${CONFIG:-$DDB_CONFIG}

mkdir -p "$OUT"

run_depth() {
  local boundaries=$1
  local breakpoint=$2
  local call_depth=$((boundaries + 1))
  python3 "$HERE/run_same_pause_depth.py" \
    --breakpoint "$breakpoint" \
    --trigger "python3 $HERE/trigger_socialnet.py --addr $ADDR --request read-user-timeline" \
    --expected-boundaries "$boundaries" \
    --ddb "$DDB" \
    --config "$CONFIG" \
    --kubeconfig "$KUBECONFIG" \
    --namespace "$NAMESPACE" \
    --selector "$(app_selector)" \
    --expected-sessions "$EXPECTED_SESSIONS" \
    --warmup "$WARMUP" \
    --repetitions "$REPETITIONS" \
    --output-dir "$OUT/depth$call_depth"
}

run_depth 1 backend_service.go:245
run_depth 2 user_timeline_service.go:28
run_depth 3 call_depth_service.go:64
run_depth 4 call_depth_service.go:68
run_depth 5 call_depth_service.go:72
run_depth 9 storage.go:263

python3 "$HERE/summarize_depth_matrix.py" \
  "2=$OUT/depth2" "3=$OUT/depth3" "4=$OUT/depth4" \
  "5=$OUT/depth5" "6=$OUT/depth6" "10=$OUT/depth10" \
  --output "$OUT/call-depth-summary.csv"

echo "Results: $OUT"
