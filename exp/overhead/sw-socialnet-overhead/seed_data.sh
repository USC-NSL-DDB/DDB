#!/bin/bash
#
# Seed the social graph (users, follows, posts) into the running socialnet app.
# Must be run after setup_experiment.sh, and again after any redeploy or scaling
# operation (pod restarts drop the in-memory state).
#
# Usage:
#   ./seed_data.sh
#   ./seed_data.sh --addr http://IP:PORT

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

BENCH_BIN="$SOCIALNET_DIR/src/bench/init_social.out"
GRAPH_FILE="$SOCIALNET_DIR/src/bench/social-graph/socfb-Reed98/socfb-Reed98.mtx"

ADDR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --addr) ADDR="$2"; shift 2 ;;
    *) die "Unknown option: $1" ;;
  esac
done

if [[ -z "$ADDR" ]]; then
  ensure_kubeconfig
  ADDR="$(detect_endpoint)"
fi

[[ -f "$BENCH_BIN" ]] || die "$BENCH_BIN not found. Run ./build_app.sh first."
[[ -f "$GRAPH_FILE" ]] || die "$GRAPH_FILE not found. Is the socialnetwork submodule initialized?"

echo "Seeding social graph at $ADDR ..."
echo "Graph file: $GRAPH_FILE"
echo ""

"$BENCH_BIN" -addr "$ADDR" -graph "$GRAPH_FILE"

echo ""
echo "Seeding complete."
