#!/usr/bin/env bash
# Validate every precondition used by the latency experiments.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

quiet=0
[[ "${1:-}" == "--quiet" ]] && quiet=1

require_command kubectl
require_command python3
require_command curl
ensure_kubeconfig
resolve_target_node
ensure_no_ddb
validate_ddb_binary
validate_runtime_config

ensure_native_k3s

read -r cluster_nodes ready_nodes target_ready <<<"$(kubectl --kubeconfig "$KUBECONFIG" get nodes -o json | python3 -c '
import json, sys
items = json.load(sys.stdin)["items"]
ready = sum(any(c["type"] == "Ready" and c["status"] == "True" for c in n["status"]["conditions"]) for n in items)
target = sys.argv[1]
target_ready = any(
    n["metadata"]["name"] == target
    and any(c["type"] == "Ready" and c["status"] == "True" for c in n["status"]["conditions"])
    for n in items
)
print(len(items), ready, int(target_ready))
' "$TARGET_NODE")"
[[ "$target_ready" -eq 1 ]] || die "target node $TARGET_NODE is not Ready"

selector="$(app_selector)"
deployments_json="$(k get deployments -l "$selector" -o json)"
read -r deployment_count single_replicas pinned_deployments <<<"$(python3 -c '
import json, sys

target = sys.argv[1]
items = json.load(sys.stdin)["items"]
single = sum(d.get("spec", {}).get("replicas") == 1 for d in items)
pinned = sum(
    d.get("spec", {}).get("template", {}).get("spec", {}).get("nodeSelector")
    == {"kubernetes.io/hostname": target}
    for d in items
)
print(len(items), single, pinned)
' "$TARGET_NODE" <<<"$deployments_json")"
[[ "$deployment_count" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS app deployments, found $deployment_count"
[[ "$single_replicas" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "only $single_replicas/$EXPECTED_DEPLOYMENTS app deployments have exactly one replica"
[[ "$pinned_deployments" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "only $pinned_deployments/$EXPECTED_DEPLOYMENTS app deployments are pinned exclusively to $TARGET_NODE"

autoscalers_json="$(k get horizontalpodautoscalers -l "$selector" -o json)"
read -r autoscaler_count fixed_autoscalers <<<"$(python3 -c '
import json, sys

items = json.load(sys.stdin)["items"]
fixed = sum(
    h.get("spec", {}).get("minReplicas") == 1
    and h.get("spec", {}).get("maxReplicas") == 1
    for h in items
)
print(len(items), fixed)
' <<<"$autoscalers_json")"
[[ "$autoscaler_count" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS app autoscalers, found $autoscaler_count"
[[ "$fixed_autoscalers" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "only $fixed_autoscalers/$EXPECTED_DEPLOYMENTS app autoscalers are fixed at one replica"

pods_json="$(k get pods -l "$selector" -o json)"

read -r total ready app_nodes local_count sidecars <<<"$(python3 -c '
import json, sys
data = json.load(sys.stdin)
target = sys.argv[1]
items = data["items"]
ready = sum(
    p.get("status", {}).get("phase") == "Running"
    and all(c.get("ready") for c in p.get("status", {}).get("containerStatuses", []))
    for p in items
)
nodes = {
    p.get("spec", {}).get("nodeName")
    for p in items
    if p.get("spec", {}).get("nodeName")
}
local = sum(p.get("spec", {}).get("nodeName") == target for p in items)
debug = sum(
    any(s.get("name", "").startswith("ssh-debugger-")
        and s.get("state", {}).get("running") is not None
        for s in p.get("status", {}).get("ephemeralContainerStatuses", []))
    for p in items
)
print(len(items), ready, len(nodes), local, debug)
' "$TARGET_NODE" <<<"$pods_json")"

[[ "$total" -eq "$EXPECTED_PROCESSES" ]] \
  || die "expected $EXPECTED_PROCESSES app pods, found $total"
[[ "$ready" -eq "$EXPECTED_PROCESSES" ]] \
  || die "only $ready/$EXPECTED_PROCESSES app pods are Ready"
[[ "$app_nodes" -eq 1 ]] \
  || die "application spans $app_nodes nodes; call depth requires exactly one deployment node"
[[ "$local_count" -eq "$EXPECTED_PROCESSES" ]] \
  || die "only $local_count/$EXPECTED_PROCESSES app pods are on $TARGET_NODE; run ./artifact.sh setup"
[[ "$sidecars" -eq "$EXPECTED_PROCESSES" ]] \
  || die "only $sidecars/$EXPECTED_PROCESSES debugger sidecars are running; run ./artifact.sh setup"

python3 "$ARTIFACT_DIR/probe_processes.py" \
  --kubeconfig "$KUBECONFIG" \
  --namespace "$NAMESPACE" \
  --selector "$selector" \
  --expected "$EXPECTED_PROCESSES" \
  --expect detached \
  --quiet

endpoint="$(detect_endpoint)"
http_code="$(curl -sS -o /dev/null --max-time 10 -w '%{http_code}' "$endpoint/" || true)"
[[ "$http_code" == "404" ]] \
  || die "application health check returned HTTP $http_code at $endpoint (expected 404)"

if [[ "$quiet" -eq 0 ]]; then
  echo "=== Single-node call-depth preflight ==="
  echo "  Kubernetes runtime: native k3s"
  echo "  Cluster nodes:      $ready_nodes/$cluster_nodes Ready"
  echo "  Deployment node:   $TARGET_NODE (Ready)"
  echo "  App deployments:   $deployment_count/$EXPECTED_DEPLOYMENTS at one replica"
  echo "  App processes:     $total/$EXPECTED_PROCESSES Ready on one node"
  echo "  Debug sidecars:     $sidecars/$EXPECTED_PROCESSES running"
  echo "  DDB binary:         $DDB_BIN"
  echo "  DDB config:         extension.py + runtime-serviceweaver.py"
  echo "  Endpoint:           $endpoint (HTTP 404 = healthy)"
  echo "  Existing DDB:       none"
  echo ""
  echo "Preflight passed."
fi
