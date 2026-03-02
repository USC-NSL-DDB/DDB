#!/bin/bash
#
# One-time experiment setup:
#   1. Join worker nodes to k3s cluster
#   2. Patch apilistener service to NodePort
#   3. Tune kernel TCP settings for high-throughput benchmarking
#   4. Rebuild bench/client binaries
#
# Usage: ./setup_experiment.sh [cluster.txt]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLUSTER_FILE="${1:-$SCRIPT_DIR/cluster.txt}"
SOCIALNET_DIR="$SCRIPT_DIR/../../fwks/socialnetwork"
MASTER_IP="10.10.1.1"

if [[ ! -f "$CLUSTER_FILE" ]]; then
  echo "Error: cluster file '$CLUSTER_FILE' not found" >&2
  exit 1
fi

echo "=== Step 1: Join worker nodes to k3s cluster ==="
"$SCRIPT_DIR/join_cluster.sh" "$CLUSTER_FILE"

echo ""
echo "=== Step 2: Wait for nodes to become Ready ==="
for i in $(seq 1 30); do
  NOT_READY=$(kubectl get nodes --no-headers 2>/dev/null | grep -v " Ready " | wc -l)
  if [[ "$NOT_READY" -eq 0 ]]; then
    echo "All nodes Ready."
    break
  fi
  echo "  Waiting for $NOT_READY node(s)... ($i/30)"
  sleep 5
done
kubectl get nodes -o wide

echo ""
echo "=== Step 3: Patch apilistener to NodePort ==="
SVC_NAME=$(kubectl get svc -o name | grep apilistener | head -1)
if [[ -z "$SVC_NAME" ]]; then
  echo "Error: no apilistener service found" >&2
  exit 1
fi
kubectl patch "$SVC_NAME" -p '{"spec":{"type":"NodePort"}}'
NODE_PORT=$(kubectl get "$SVC_NAME" -o jsonpath='{.spec.ports[0].nodePort}')
echo "NodePort assigned: $NODE_PORT"
echo "Endpoint: http://${MASTER_IP}:${NODE_PORT}"

echo ""
echo "=== Step 4: Tune kernel TCP settings ==="
sudo sysctl -w net.ipv4.tcp_tw_reuse=1
sudo sysctl -w net.ipv4.ip_local_port_range="1024 65535"
sudo sysctl -w net.ipv4.tcp_fin_timeout=15

echo ""
echo "=== Step 5: Build benchmark binaries ==="
pushd "$SOCIALNET_DIR/src/bench" > /dev/null
go build -o init_social.out .
echo "  Built init_social.out"
popd > /dev/null

pushd "$SOCIALNET_DIR/src/client" > /dev/null
go build -o client.out .
echo "  Built client.out"
popd > /dev/null

echo ""
echo "=== Setup complete ==="
echo "Endpoint: http://${MASTER_IP}:${NODE_PORT}"
echo ""
echo "Next steps:"
echo "  1. Seed data:  ./seed_data.sh"
echo "  2. Benchmark:  ./run_benchmark.sh --target-mops 0.00005"
