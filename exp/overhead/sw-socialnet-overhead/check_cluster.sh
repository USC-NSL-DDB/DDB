#!/bin/bash
#
# Health check for the k3s cluster and socialnet deployment.
#
# Usage: ./check_cluster.sh [cluster.txt]

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

CLUSTER_FILE="${1:-$EXP_DIR/cluster.txt}"

echo "=== Node Reachability ==="
while IFS= read -r ip || [[ -n "$ip" ]]; do
  [[ -z "$ip" || "$ip" == \#* ]] && continue
  ping -c 1 -W 2 "$ip" > /dev/null 2>&1 && echo "  $ip: reachable" || echo "  $ip: UNREACHABLE"
done <"$CLUSTER_FILE"

ensure_kubeconfig

echo ""
echo "=== K3s Nodes ==="
kubectl get nodes -o wide 2>&1

echo ""
echo "=== App Pods ==="
kubectl get pods -o wide -n "$NAMESPACE" --sort-by='.spec.nodeName' 2>&1

echo ""
echo "=== API Endpoint ==="
svc_name="$(apilistener_svc)"
if [[ -n "$svc_name" ]]; then
  svc_type=$(kubectl get "$svc_name" -n "$NAMESPACE" -o jsonpath='{.spec.type}')
  if [[ "$svc_type" == "NodePort" ]]; then
    url="$(detect_endpoint)"
    echo "  Type: NodePort"
    echo "  URL:  $url"
    # --max-time: a broken pod network makes this hang forever otherwise.
    http_code=$(curl -s -o /dev/null --max-time 10 -w "%{http_code}" "$url/" 2>/dev/null)
    if [[ "$http_code" == "000" ]]; then
      echo "  Health: UNREACHABLE (timed out)"
      echo "    The apilistener endpoint is on a pod that node0 cannot reach."
      echo "    Check the overlay: kubectl get nodes -o wide (InternalIP must be ${MASTER_IP%.*}.x)"
    else
      echo "  Health: GET / -> HTTP $http_code (404 = healthy, app has no root route)"
    fi
  else
    echo "  Type: $svc_type (not NodePort — run setup_experiment.sh)"
  fi
else
  echo "  No apilistener service found (run ./deploy_app.sh)"
fi

echo ""
echo "=== DDB Sidecars ==="
app_label="$(app_label_value)"
if [[ -n "$app_label" ]]; then
  "$EXP_DIR/setup_ddb.sh" --check 2>/dev/null | tail -n 2 || echo "  none injected (run ./setup_ddb.sh)"
else
  echo "  app not deployed"
fi

echo ""
echo "=== TCP State ==="
ss -s 2>/dev/null | head -3
