#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 3 ]]; then
  echo "usage: $0 DDB_BINARY [OUTPUT_JSON] [ITERATIONS]" >&2
  exit 2
fi

ddb_binary=$(realpath "$1")
output_json=${2:-ddb/benchmarks/evidence/2026-08-15-tui-dispatch/result.json}
iterations=${3:-100}
if [[ ! -x "$ddb_binary" ]]; then
  echo "DDB binary is not executable: $ddb_binary" >&2
  exit 2
fi
if [[ ! "$iterations" =~ ^[0-9]+$ ]] || (( iterations < 20 )); then
  echo "ITERATIONS must be an integer of at least 20" >&2
  exit 2
fi

measurement_dir=$(mktemp -d)
trap 'rm -rf -- "$measurement_dir"' EXIT
fake_tui="$measurement_dir/ddb-tui"
direct_samples="$measurement_dir/direct.ns"
dispatch_samples="$measurement_dir/dispatch.ns"
printf '#!/bin/sh\nexit 0\n' >"$fake_tui"
chmod 700 "$fake_tui"

for _ in $(seq 1 10); do
  "$fake_tui"
  DDB_TUI_PATH="$fake_tui" "$ddb_binary" tui
done

for _ in $(seq 1 "$iterations"); do
  start_ns=$(date +%s%N)
  "$fake_tui"
  end_ns=$(date +%s%N)
  echo $((end_ns - start_ns)) >>"$direct_samples"

  start_ns=$(date +%s%N)
  DDB_TUI_PATH="$fake_tui" "$ddb_binary" tui
  end_ns=$(date +%s%N)
  echo $((end_ns - start_ns)) >>"$dispatch_samples"
done

sort -n "$direct_samples" -o "$direct_samples"
sort -n "$dispatch_samples" -o "$dispatch_samples"
p95_rank=$(((iterations * 95 + 99) / 100))
direct_p95_ns=$(sed -n "${p95_rank}p" "$direct_samples")
dispatch_p95_ns=$(sed -n "${p95_rank}p" "$dispatch_samples")
overhead_p95_ns=$((dispatch_p95_ns - direct_p95_ns))
if (( overhead_p95_ns < 0 )); then
  overhead_p95_ns=0
fi
gate_ns=100000000
mkdir -p "$(dirname "$output_json")"
printf '{\n  "schema_version": 1,\n  "iterations": %s,\n  "direct_p95_ns": %s,\n  "dispatch_p95_ns": %s,\n  "overhead_p95_ns": %s,\n  "gate_ns": %s,\n  "passed": %s\n}\n' \
  "$iterations" "$direct_p95_ns" "$dispatch_p95_ns" "$overhead_p95_ns" "$gate_ns" \
  "$([[ $overhead_p95_ns -le $gate_ns ]] && echo true || echo false)" >"$output_json"

cat "$output_json"
if (( overhead_p95_ns > gate_ns )); then
  echo "ddb tui dispatcher overhead exceeded 100 ms p95" >&2
  exit 1
fi
