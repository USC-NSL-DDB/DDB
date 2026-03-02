#!/bin/bash
#
# Redeploy the socialnet app to get fresh pods (clears in-memory state).
#
# Modes:
#   ./redeploy_app.sh              # Rolling restart (default): new pods, same config, fast
#   ./redeploy_app.sh --full       # Full teardown + re-apply from saved manifests
#
# After redeploy, the service is re-patched to NodePort and you need to re-seed:
#   ./seed_data.sh
#
# Note: --full mode requires socialnet-manifests.yaml (exported by setup_experiment.sh
# or manually via: kubectl get deployments,services,hpa -o yaml > socialnet-manifests.yaml)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFESTS="$SCRIPT_DIR/socialnet-manifests.yaml"
MASTER_IP="10.10.1.1"
MODE="restart"

if [[ "${1:-}" == "--full" ]]; then
  MODE="full"
fi

wait_for_pods() {
  echo "Waiting for all pods to be Ready..."
  for i in $(seq 1 60); do
    NOT_READY=$(kubectl get pods -n default --no-headers 2>/dev/null \
      | grep -v "ssh-gateway" \
      | grep -v "1/1.*Running" \
      | wc -l)
    if [[ "$NOT_READY" -eq 0 ]]; then
      echo "All app pods Ready."
      return 0
    fi
    echo "  $NOT_READY pod(s) not ready... ($i/60)"
    sleep 5
  done
  echo "Warning: some pods still not ready after 5 minutes" >&2
  return 1
}

patch_nodeport() {
  local svc_name
  svc_name=$(kubectl get svc -o name 2>/dev/null | grep apilistener | head -1)
  if [[ -n "$svc_name" ]]; then
    kubectl patch "$svc_name" -p '{"spec":{"type":"NodePort"}}' 2>/dev/null || true
    local node_port
    node_port=$(kubectl get "$svc_name" -o jsonpath='{.spec.ports[0].nodePort}')
    echo "Endpoint: http://${MASTER_IP}:${node_port}"
  fi
}

if [[ "$MODE" == "restart" ]]; then
  echo "=== Rolling restart (fresh pods, same config) ==="
  kubectl get deployments -n default -o name \
    | grep -v ssh-gateway \
    | xargs -I {} kubectl rollout restart {} -n default
  echo "Rollout restart triggered for all app deployments."
  echo ""
  wait_for_pods
  echo ""
  patch_nodeport

elif [[ "$MODE" == "full" ]]; then
  echo "=== Full teardown + re-apply ==="

  if [[ ! -f "$MANIFESTS" ]]; then
    echo "Error: $MANIFESTS not found." >&2
    echo "Export current state first:" >&2
    echo "  kubectl get deployments,services,hpa -o yaml -n default > socialnet-manifests.yaml" >&2
    exit 1
  fi

  echo "Deleting all app deployments, services, HPAs..."
  kubectl get deployments -n default -o name \
    | grep -v ssh-gateway \
    | xargs kubectl delete -n default --wait=true 2>/dev/null || true
  kubectl get hpa -n default -o name \
    | xargs kubectl delete -n default --wait=true 2>/dev/null || true
  kubectl get svc -n default -o name \
    | grep -v kubernetes \
    | grep -v ssh-gateway \
    | xargs kubectl delete -n default --wait=true 2>/dev/null || true

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
kubectl get pods -o wide -n default --sort-by='.spec.nodeName' \
  | grep -v ssh-gateway
echo ""
echo "Next: re-seed data with ./seed_data.sh"
