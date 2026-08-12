#!/bin/bash
#
# Measure the per-call latency of gettimeofday / clock_gettime(REALTIME) /
# clock_gettime(MONOTONIC) with and without libfaketime interposition:
#
#   none      no LD_PRELOAD                      -- the vDSO fast path
#   faketime  libfaketime, FAKETIME="+0"         -- interposed
#
# The offset is zero -- the experiment isolates what interception itself
# costs, not any particular fake offset. One bench_time process per config
# (LD_PRELOAD is process-wide). Each process verifies whether libfaketime is
# really mapped and this script cross-checks it against what it asked for, so
# a silently-failed LD_PRELOAD cannot masquerade as "no overhead".
#
#   ./run_benchmark.sh [--cpu N]     # pin the benchmark to core N (default 2)

set -uo pipefail
EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
[[ -z "$REPO_ROOT" ]] && REPO_ROOT="$(cd "$EXP_DIR/../../.." && pwd)"

LIBFAKETIME_SO="$REPO_ROOT/libfaketime/src/libfaketime.so.1"
RESULTS_DIR="$EXP_DIR/results"
BENCH="$EXP_DIR/bench_time"

die() { echo "Error: $*" >&2; exit 1; }

CPU=2
while [[ $# -gt 0 ]]; do
  case "$1" in
    --cpu) CPU="$2"; shift 2 ;;
    *) die "Unknown option: $1" ;;
  esac
done

[[ -x "$BENCH" ]]          || die "bench_time not built. Run ./build_all.sh"
[[ -f "$LIBFAKETIME_SO" ]] || die "libfaketime not built. Run ./build_all.sh"

mkdir -p "$RESULTS_DIR"
RESULT="$RESULTS_DIR/faketime_overhead_$(date +%Y%m%d_%H%M%S).txt"

gov=$(cat "/sys/devices/system/cpu/cpu$CPU/cpufreq/scaling_governor" 2>/dev/null || echo unknown)
[[ "$gov" == "performance" || "$gov" == "unknown" ]] \
  || echo "warning: cpu$CPU governor is '$gov', not 'performance'; frequency scaling adds noise" >&2

# ─── One process per config ──────────────────────────────────────────────────
run_config() {  # $1 = label, $2 = expect-interposed yes|no, rest = env pairs
  local label="$1" expect="$2"; shift 2
  local out
  out=$(env "$@" "$BENCH" "$CPU" 2>"$RESULTS_DIR/.stderr.$label") \
    || die "bench_time failed under config '$label'; see $RESULTS_DIR/.stderr.$label"

  local interposed
  interposed=$(awk '/^INTERPOSED/{print $2}' <<<"$out")
  [[ "$interposed" == "$expect" ]] \
    || die "config '$label': expected interposed=$expect but the process reports '$interposed'
  (LD_PRELOAD silently failing -- or leaking into the baseline -- would fake a result)"

  # tag each RESULT line with the config
  awk -v cfg="$label" '/^RESULT/{print cfg, $0}' <<<"$out"
}

echo "=== libfaketime interposition overhead (cpu $CPU, offset +0) ===" >&2
{
  echo "# config api min_ns median_ns mean_ns  (per call)"
  run_config none     no
  run_config faketime yes LD_PRELOAD="$LIBFAKETIME_SO" FAKETIME="+0"
} | tee "$RESULT"

# ─── Comparison table ────────────────────────────────────────────────────────
echo ""
echo "=== Summary (median ns/call; overhead vs none) ==="
awk '
  /^[a-z]+ RESULT/ {
    split($3, a, "="); api = a[2]
    split($5, m, "="); med = m[2]
    v[$1 "," api] = med
    if (!(api in seen)) { order[++n] = api; seen[api] = 1 }
  }
  END {
    printf "%-26s %10s %10s %14s\n", "api", "none", "faketime", "overhead"
    for (i = 1; i <= n; i++) {
      api = order[i]
      base = v["none," api]; f = v["faketime," api]
      printf "%-26s %10.1f %10.1f %13.1fx\n", api, base, f, f / base
    }
  }' "$RESULT"

echo ""
echo "Result: $RESULT"
