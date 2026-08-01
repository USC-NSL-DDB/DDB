#!/usr/bin/env bash
# Run repeated DBT for every thread while one global pause is held.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

repetitions="${COMMAND_REPETITIONS:-10}"
warmup="${COMMAND_WARMUP_PASSES:-1}"
thread_limit="${COMMAND_THREAD_LIMIT:-0}"
command_workers="$COMMAND_WORKERS"
output=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --smoke)
      repetitions=2
      warmup=1
      thread_limit=1
      shift
      ;;
    --repetitions) repetitions="$2"; shift 2 ;;
    --warmup-passes) warmup="$2"; shift 2 ;;
    --thread-limit) thread_limit="$2"; shift 2 ;;
    --command-workers) command_workers="$2"; shift 2 ;;
    --output-dir) output="$2"; shift 2 ;;
    -h|--help)
      cat <<EOF
Usage: $0 [--smoke] [--repetitions N] [--warmup-passes N]
          [--thread-limit N] [--command-workers N] [--output-dir DIR]
EOF
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

"$ARTIFACT_DIR/check_cluster.sh" --quiet
output="${output:-$RESULTS_ROOT/command-$(timestamp)}"
mkdir -p "$output"
output="$(cd "$output" && pwd)"

postcheck_detached() {
  local status=$?
  trap - EXIT
  if ! python3 "$ARTIFACT_DIR/probe_processes.py" \
    --kubeconfig "$KUBECONFIG" \
    --namespace "$NAMESPACE" \
    --selector "$(app_selector)" \
    --debugger-prefix "$DEBUGGER_CONTAINER_PREFIX" \
    --expected "$EXPECTED_PROCESSES" \
    --expect detached \
    --quiet; then
    echo "Error: post-run kernel detach verification failed" >&2
    status=1
  fi
  exit "$status"
}
trap postcheck_detached EXIT

ddb_revision="$(git -C "$DDB_REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
socialnet_revision="$(git -C "$SOCIALNET_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"

[[ "$command_workers" =~ ^[1-9][0-9]*$ ]] \
  || die "command workers must be a positive integer, got '$command_workers'"

note "Command latency: concurrent-batch warmup=$warmup repetitions=$repetitions thread-limit=$thread_limit"
python3 "$ARTIFACT_DIR/run_command_latency.py" \
  --ddb "$DDB_BIN" \
  --config "$DDB_CONFIG" \
  --kubeconfig "$KUBECONFIG" \
  --namespace "$NAMESPACE" \
  --selector "$(app_selector)" \
  --debugger-prefix "$DEBUGGER_CONTAINER_PREFIX" \
  --ddb-revision "$ddb_revision" \
  --socialnet-revision "$socialnet_revision" \
  --expected-sessions "$EXPECTED_PROCESSES" \
  --warmup-passes "$warmup" \
  --repetitions "$repetitions" \
  --thread-limit "$thread_limit" \
  --command-workers "$command_workers" \
  --output-dir "$output"

mkdir -p "$RESULTS_ROOT"
ln -sfn "$output" "$RESULTS_ROOT/latest-command"
ln -sfn "$output" "$RESULTS_ROOT/latest"
show_csv "$output/summary.csv"

echo ""
echo "Command-latency result: $output"
