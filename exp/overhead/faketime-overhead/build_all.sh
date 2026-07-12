#!/bin/bash
#
# Build what the libfaketime overhead experiment needs:
#   1. libfaketime -- the DDB-shipped copy in this repo (the same .so the PET
#      experiment and DDB's gdb extension preload), not a packaged one
#   2. bench_time  -- the measurement binary (calibrated-rdtsc loop timer)
#
# Single node; no cluster setup.

set -euo pipefail
EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
[[ -z "$REPO_ROOT" ]] && REPO_ROOT="$(cd "$EXP_DIR/../../.." && pwd)"

LIBFAKETIME_DIR="$REPO_ROOT/libfaketime"
LIBFAKETIME_SO="$LIBFAKETIME_DIR/src/libfaketime.so.1"

die() { echo "Error: $*" >&2; exit 1; }

echo "=== 1/2 libfaketime ==="
make -C "$LIBFAKETIME_DIR/src" all >/dev/null
[[ -f "$LIBFAKETIME_SO" ]] || die "libfaketime build produced no $LIBFAKETIME_SO"
echo "  $LIBFAKETIME_SO"

echo ""
echo "=== 2/2 bench_time ==="
# -O2 but the measured calls write through a volatile sink, so the loop bodies
# survive optimization. No LTO: keep the loop structure predictable.
gcc -O2 -std=gnu11 -Wall -Wextra -o "$EXP_DIR/bench_time" "$EXP_DIR/bench_time.c"
echo "  $EXP_DIR/bench_time"

echo ""
echo "=== Build complete ==="
echo "Next: ./run_benchmark.sh"
