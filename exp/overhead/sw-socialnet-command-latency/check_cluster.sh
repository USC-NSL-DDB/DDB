#!/usr/bin/env bash
# Validate the complete multi-node command-latency topology.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

quiet=0
[[ "${1:-}" == "--quiet" ]] && quiet=1

require_command kubectl
require_command python3
require_command curl
require_command ip
validate_cluster_inputs
ensure_kubeconfig
resolve_target_node
ensure_no_ddb
validate_ddb_binary
validate_runtime_config
ensure_native_k3s

read -r cluster_nodes ready_nodes <<<"$(kubectl --kubeconfig "$KUBECONFIG" get nodes -o json | python3 -c '
import json, sys
items = json.load(sys.stdin)["items"]
ready = sum(any(c["type"] == "Ready" and c["status"] == "True" for c in n["status"]["conditions"]) for n in items)
print(len(items), ready)
')"
[[ "$cluster_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "expected $EXPECTED_CLUSTER_NODES k3s nodes, found $cluster_nodes"
[[ "$ready_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "only $ready_nodes/$EXPECTED_CLUSTER_NODES k3s nodes are Ready"

read -r worker_nodes controller_nodes controller_internal_ip <<<"$(
  kubectl --kubeconfig "$KUBECONFIG" get nodes -o json | python3 -c '
import json, sys
items = json.load(sys.stdin)["items"]
workers = sum(n.get("metadata", {}).get("labels", {}).get("ddb-artifact-role") == "worker" for n in items)
controllers = [n for n in items if n.get("metadata", {}).get("labels", {}).get("ddb-artifact-role") == "controller"]
addresses = []
if len(controllers) == 1:
    addresses = [a["address"] for a in controllers[0].get("status", {}).get("addresses", []) if a.get("type") == "InternalIP"]
print(workers, len(controllers), addresses[0] if addresses else "missing")
' )"
[[ "$worker_nodes" -eq "$EXPECTED_WORKERS" ]] \
  || die "expected $EXPECTED_WORKERS recipe-labeled workers, found $worker_nodes"
[[ "$controller_nodes" -eq 1 ]] \
  || die "expected one recipe-labeled controller, found $controller_nodes"
[[ "$controller_internal_ip" == "$CONTROLLER_IP" ]] \
  || die "controller InternalIP is $controller_internal_ip, expected $CONTROLLER_IP"

selector="$(app_selector)"
pods_json="$(k get pods -l "$selector" -o json)"
read -r total ready sidecars app_nodes control_pods <<<"$(python3 -c '
import json, sys
data = json.load(sys.stdin)
control = sys.argv[1]
prefix = sys.argv[2]
items = data["items"]
ready = sum(
    p.get("status", {}).get("phase") == "Running"
    and all(c.get("ready") for c in p.get("status", {}).get("containerStatuses", []))
    for p in items
)
debug = sum(
    any(s.get("name", "").startswith(prefix)
        and s.get("state", {}).get("running") is not None
        for s in p.get("status", {}).get("ephemeralContainerStatuses", []))
    for p in items
)
nodes = {p.get("spec", {}).get("nodeName") for p in items}
on_control = sum(p.get("spec", {}).get("nodeName") == control for p in items)
print(len(items), ready, debug, len(nodes), on_control)
' "$TARGET_NODE" "$DEBUGGER_CONTAINER_PREFIX" <<<"$pods_json")"

[[ "$total" -eq "$EXPECTED_PROCESSES" ]] || die "expected $EXPECTED_PROCESSES app pods, found $total"
[[ "$ready" -eq "$EXPECTED_PROCESSES" ]] || die "only $ready/$EXPECTED_PROCESSES app pods are Ready"
[[ "$sidecars" -eq "$EXPECTED_PROCESSES" ]] \
  || die "only $sidecars/$EXPECTED_PROCESSES debugger sidecars are running; run ./artifact.sh setup"
[[ "$app_nodes" -eq "$EXPECTED_APP_NODES" ]] \
  || die "application spans $app_nodes nodes; expected $EXPECTED_APP_NODES of $EXPECTED_WORKERS configured workers"
[[ "$control_pods" -eq 0 ]] \
  || die "$control_pods application pods are on control node $TARGET_NODE; run ./artifact.sh setup"

python3 "$ARTIFACT_DIR/probe_processes.py" \
  --kubeconfig "$KUBECONFIG" --namespace "$NAMESPACE" --selector "$selector" \
  --debugger-prefix "$DEBUGGER_CONTAINER_PREFIX" \
  --expected "$EXPECTED_PROCESSES" --expect detached --quiet

endpoint="$(detect_endpoint)"
http_code="$(curl -sS -o /dev/null --max-time 10 -w '%{http_code}' "$endpoint/" || true)"
[[ "$http_code" == "404" ]] || die "application health check returned HTTP $http_code at $endpoint"

if [[ "$quiet" -eq 0 ]]; then
  echo "=== Full-cluster command-latency preflight ==="
  echo "  Cluster nodes:       $ready_nodes/$EXPECTED_CLUSTER_NODES Ready"
  echo "  Worker nodes:        $worker_nodes/$EXPECTED_WORKERS Ready"
  echo "  Application nodes:   $app_nodes/$EXPECTED_APP_NODES expected workers"
  echo "  App processes:       $ready/$EXPECTED_PROCESSES Ready"
  echo "  Control-node pods:   0"
  echo "  Debug sidecars:      $sidecars/$EXPECTED_PROCESSES running"
  echo "  Kernel state:        all detached"
  echo "  Endpoint:            $endpoint (HTTP 404 = healthy)"
  echo ""
  echo "Preflight passed."
fi
