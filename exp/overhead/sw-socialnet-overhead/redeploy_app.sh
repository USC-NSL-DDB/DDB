#!/bin/bash
#
# Redeploy the socialnet app to get fresh pods (clears in-memory state).
#
# Modes:
#   ./redeploy_app.sh              # Rolling restart (default): new pods, same config, fast
#   ./redeploy_app.sh --full       # Full teardown + re-apply from saved manifests
#
# After redeploy the service is re-patched to NodePort. You must then re-seed:
#   ./seed_data.sh
# and, if you use DDB, re-inject sidecars (pod restarts drop them):
#   ./setup_ddb.sh
#
# --full mode replays socialnet-manifests.yaml, exported by ./deploy_app.sh.
# To deploy the app for the first time, use ./deploy_app.sh instead.

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

MANIFESTS="$EXP_DIR/socialnet-manifests.yaml"
MODE="restart"

if [[ "${1:-}" == "--full" ]]; then
  MODE="full"
fi

ensure_kubeconfig

if [[ "$MODE" == "restart" ]]; then
  echo "=== Rolling restart (fresh pods, same config) ==="
  kubectl get deployments -n "$NAMESPACE" -o name \
    | grep -v ssh-gateway \
    | xargs -I {} kubectl rollout restart {} -n "$NAMESPACE"
  echo "Rollout restart triggered for all app deployments."
  echo ""
  wait_for_pods
  echo ""
  patch_nodeport

elif [[ "$MODE" == "full" ]]; then
  echo "=== Full teardown + re-apply ==="

  [[ -f "$MANIFESTS" ]] || die "$MANIFESTS not found.
  Export current state first:
    kubectl get deployments,services,hpa -o yaml -n $NAMESPACE > socialnet-manifests.yaml"

  echo "Deleting all app deployments, services, HPAs..."
  kubectl get deployments -n "$NAMESPACE" -o name \
    | grep -v ssh-gateway \
    | xargs kubectl delete -n "$NAMESPACE" --wait=true 2>/dev/null || true
  kubectl get hpa -n "$NAMESPACE" -o name \
    | xargs kubectl delete -n "$NAMESPACE" --wait=true 2>/dev/null || true
  kubectl get svc -n "$NAMESPACE" -o name \
    | grep -v kubernetes \
    | grep -v ssh-gateway \
    | xargs kubectl delete -n "$NAMESPACE" --wait=true 2>/dev/null || true

  echo "Waiting for pods to terminate..."
  sleep 10

  echo "Re-applying manifests..."
  kubectl apply -f "$MANIFESTS"
  echo ""
  wait_for_pods
  echo ""
  patch_nodeport
fi

echo ""
echo "=== Redeploy complete ==="
echo "Pod distribution:"
kubectl get pods -o wide -n "$NAMESPACE" --sort-by='.spec.nodeName' | grep -v ssh-gateway || true
echo ""
echo "Next: re-seed data with ./seed_data.sh"
