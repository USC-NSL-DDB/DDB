#!/bin/bash
#
# One-command experiment setup. Run this on the master node (node0) after
# ./deploy_all.sh has installed dependencies everywhere.
#
#   1. Start the k3s server and make its kubeconfig usable without sudo
#   2. Join worker nodes to the k3s cluster
#   3. Build the socialnet binaries (docker, go1.21.1)
#   4. Deploy the app with weaver-kube + kubectl apply
#   5. Expose apilistener as a NodePort
#   6. Tune kernel TCP settings for high-throughput benchmarking
#
# Every step is idempotent: re-running skips work that is already done.
#
# Usage:
#   ./setup_experiment.sh
#   ./setup_experiment.sh --skip-build      # binaries already built
#   ./setup_experiment.sh --skip-deploy     # app already running
#   ./setup_experiment.sh --cluster other.txt

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

CLUSTER_FILE="$EXP_DIR/cluster.txt"
SKIP_BUILD=0
SKIP_DEPLOY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build)  SKIP_BUILD=1;  shift ;;
    --skip-deploy) SKIP_DEPLOY=1; shift ;;
    --cluster)     CLUSTER_FILE="$2"; shift 2 ;;
    -*) die "Unknown option: $1" ;;
    *)  CLUSTER_FILE="$1"; shift ;;   # backwards compat: ./setup_experiment.sh cluster.txt
  esac
done

[[ -f "$CLUSTER_FILE" ]] || die "cluster file '$CLUSTER_FILE' not found"

echo "=== Step 1: k3s server + kubeconfig ==="
ensure_k3s_server
ensure_kubeconfig
echo "Using KUBECONFIG=$KUBECONFIG"

echo ""
echo "=== Step 2: Join worker nodes to k3s cluster ==="
"$EXP_DIR/join_cluster.sh" "$CLUSTER_FILE"

echo ""
echo "=== Step 3: Wait for nodes to become Ready ==="
wait_for_nodes "$CLUSTER_FILE"

prune_stale_nodes "$CLUSTER_FILE"
taint_master
kubectl get nodes -o wide

EXPECTED=$(grep -cvE '^\s*(#|$)' "$CLUSTER_FILE")
ACTUAL=$(kubectl get nodes --no-headers 2>/dev/null | wc -l)
if [[ "$ACTUAL" -ne "$EXPECTED" ]]; then
  die "expected $EXPECTED nodes, found $ACTUAL after pruning."
fi

echo ""
echo "=== Step 4: Build socialnet binaries ==="
if [[ "$SKIP_BUILD" -eq 1 ]]; then
  echo "Skipped (--skip-build)."
else
  "$EXP_DIR/build_app.sh"
fi

echo ""
echo "=== Step 5: Deploy app to Kubernetes ==="
if [[ "$SKIP_DEPLOY" -eq 1 ]]; then
  echo "Skipped (--skip-deploy)."
  app_is_deployed || die "app is not deployed and --skip-deploy was passed"
else
  "$EXP_DIR/deploy_app.sh"
fi

echo ""
echo "=== Step 6: Expose apilistener as NodePort ==="
patch_nodeport

echo ""
echo "=== Step 7: Tune kernel TCP settings ==="
sudo sysctl -w net.ipv4.tcp_tw_reuse=1
sudo sysctl -w net.ipv4.ip_local_port_range="1024 65535"
sudo sysctl -w net.ipv4.tcp_fin_timeout=15

echo ""
echo "=== Setup complete ==="
echo "Endpoint: $(detect_endpoint)"
echo ""
echo "Next steps:"
echo "  1. Seed data:  ./seed_data.sh"
echo "  2. Benchmark:  ./run_benchmark.sh --target-mops 0.00005"
echo "  3. (DDB only): ./setup_ddb.sh"
