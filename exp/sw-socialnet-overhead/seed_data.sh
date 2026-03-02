#!/bin/bash
#
# Seed the social graph (users, follows, posts) into the running socialnet app.
# Must be run after setup_experiment.sh. Only needs to run once per deployment.
#
# Usage: ./seed_data.sh [--addr http://IP:PORT]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOCIALNET_DIR="$SCRIPT_DIR/../../fwks/socialnetwork"
BENCH_BIN="$SOCIALNET_DIR/src/bench/init_social.out"
GRAPH_FILE="$SOCIALNET_DIR/src/bench/social-graph/socfb-Reed98/socfb-Reed98.mtx"

# Auto-detect endpoint from k8s
detect_endpoint() {
  local master_ip="10.10.1.1"
  local svc_name
  svc_name=$(kubectl get svc -o name 2>/dev/null | grep apilistener | head -1)
  if [[ -z "$svc_name" ]]; then
    echo "Error: no apilistener service found. Run setup_experiment.sh first." >&2
    exit 1
  fi
  local node_port
  node_port=$(kubectl get "$svc_name" -o jsonpath='{.spec.ports[0].nodePort}')
  echo "http://${master_ip}:${node_port}"
}

ADDR="${1:-}"
if [[ "$ADDR" == "--addr" ]]; then
  ADDR="$2"
elif [[ -z "$ADDR" ]]; then
  ADDR=$(detect_endpoint)
fi

if [[ ! -f "$BENCH_BIN" ]]; then
  echo "Error: $BENCH_BIN not found. Run setup_experiment.sh first." >&2
  exit 1
fi

echo "Seeding social graph at $ADDR ..."
echo "Graph file: $GRAPH_FILE"
echo ""

"$BENCH_BIN" -addr "$ADDR" -graph "$GRAPH_FILE"

echo ""
echo "Seeding complete."
