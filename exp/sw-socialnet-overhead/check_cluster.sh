#!/bin/bash
#
# Health check for the k3s cluster and socialnet deployment.
#
# Usage: ./check_cluster.sh [cluster.txt]

CLUSTER_FILE="${1:-cluster.txt}"

echo "=== Node Reachability ==="
while IFS= read -r ip || [[ -n "$ip" ]]; do
  [[ -z "$ip" || "$ip" == \#* ]] && continue
  ping -c 1 -W 2 "$ip" > /dev/null 2>&1 && echo "  $ip: reachable" || echo "  $ip: UNREACHABLE"
done <"$CLUSTER_FILE"

echo ""
echo "=== K3s Nodes ==="
kubectl get nodes -o wide 2>&1

echo ""
echo "=== App Pods ==="
kubectl get pods -o wide -n default --sort-by='.spec.nodeName' 2>&1

echo ""
echo "=== API Endpoint ==="
svc_name=$(kubectl get svc -o name 2>/dev/null | grep apilistener | head -1)
if [[ -n "$svc_name" ]]; then
  svc_type=$(kubectl get "$svc_name" -o jsonpath='{.spec.type}')
  if [[ "$svc_type" == "NodePort" ]]; then
    node_port=$(kubectl get "$svc_name" -o jsonpath='{.spec.ports[0].nodePort}')
    echo "  Type: NodePort"
    echo "  URL:  http://10.10.1.1:${node_port}"
    http_code=$(curl -s -o /dev/null -w "%{http_code}" "http://10.10.1.1:${node_port}/" 2>/dev/null)
    echo "  Health: GET / -> HTTP $http_code (404 = healthy, app has no root route)"
  else
    echo "  Type: $svc_type (not NodePort — run setup_experiment.sh)"
  fi
else
  echo "  No apilistener service found"
fi

echo ""
echo "=== TCP State ==="
ss -s 2>/dev/null | head -3
