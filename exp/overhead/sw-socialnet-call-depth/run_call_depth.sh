#!/usr/bin/env bash
# Run the validated DBT matrix at call depths 2, 3, 4, 5, 6, and 10.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

repetitions="${DEPTH_REPETITIONS:-30}"
warmup="${DEPTH_WARMUP:-3}"
output=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --smoke)
      repetitions=3
      warmup=1
      shift
      ;;
    --repetitions) repetitions="$2"; shift 2 ;;
    --warmup) warmup="$2"; shift 2 ;;
    --output-dir) output="$2"; shift 2 ;;
    -h|--help)
      cat <<EOF
Usage: $0 [--smoke] [--repetitions N] [--warmup N] [--output-dir DIR]
EOF
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

"$ARTIFACT_DIR/check_cluster.sh" --quiet
output="${output:-$RESULTS_ROOT/call-depth-$(timestamp)}"
mkdir -p "$output"
output="$(cd "$output" && pwd)"

postcheck_detached() {
  local status=$?
  trap - EXIT
  if ! python3 "$ARTIFACT_DIR/probe_processes.py" \
    --kubeconfig "$KUBECONFIG" \
    --namespace "$NAMESPACE" \
    --selector "$(app_selector)" \
    --expected "$EXPECTED_PROCESSES" \
    --expect detached \
    --quiet; then
    echo "Error: post-run kernel detach verification failed" >&2
    status=1
  fi
  exit "$status"
}
trap postcheck_detached EXIT

{
  echo "ddb_commit=$(git -C "$DDB_REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "socialnet_commit=$(git -C "$SOCIALNET_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
} > "$output/source-revisions.txt"

note "Call depth: depths=2,3,4,5,6,10 warmup=$warmup repetitions=$repetitions"
ADDR="$(detect_endpoint)" \
OUT="$output" \
WARMUP="$warmup" \
REPETITIONS="$repetitions" \
EXPECTED_SESSIONS="$EXPECTED_PROCESSES" \
DDB="$DDB_BIN" \
CONFIG="$DDB_CONFIG" \
KUBECONFIG="$KUBECONFIG" \
  "$ARTIFACT_DIR/run_depth_matrix.sh"

mkdir -p "$RESULTS_ROOT"
ln -sfn "$output" "$RESULTS_ROOT/latest-depth"
ln -sfn "$output" "$RESULTS_ROOT/latest"
show_csv "$output/call-depth-summary.csv"

echo ""
echo "Call-depth result: $output"
