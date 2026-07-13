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

read -r cluster_nodes ready_nodes <<<"$(kubectl --kubeconfig "$KUBECONFIG" get nodes -o json | python3 -c '
import json, sys
items = json.load(sys.stdin)["items"]
ready = sum(any(c["type"] == "Ready" and c["status"] == "True" for c in n["status"]["conditions"]) for n in items)
print(len(items), ready)
')"
[[ "$cluster_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "call depth requires exactly one k3s node; found $cluster_nodes"
[[ "$ready_nodes" -eq 1 ]] || die "the single k3s node is not Ready"

selector="$(app_selector)"
pods_json="$(k get pods -l "$selector" -o json)"

read -r total ready local_count sidecars <<<"$(python3 -c '
import json, sys
data = json.load(sys.stdin)
target = sys.argv[1]
items = data["items"]
ready = sum(
    p.get("status", {}).get("phase") == "Running"
    and all(c.get("ready") for c in p.get("status", {}).get("containerStatuses", []))
    for p in items
)
local = sum(p.get("spec", {}).get("nodeName") == target for p in items)
debug = sum(
    any(s.get("name", "").startswith("ssh-debugger-")
        and s.get("state", {}).get("running") is not None
        for s in p.get("status", {}).get("ephemeralContainerStatuses", []))
    for p in items
)
print(len(items), ready, local, debug)
' "$TARGET_NODE" <<<"$pods_json")"

[[ "$total" -eq "$EXPECTED_PROCESSES" ]] \
  || die "expected $EXPECTED_PROCESSES app pods, found $total"
[[ "$ready" -eq "$EXPECTED_PROCESSES" ]] \
  || die "only $ready/$EXPECTED_PROCESSES app pods are Ready"
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
  echo "=== Single-host call-depth preflight ==="
  echo "  Kubernetes runtime: native k3s on one physical host"
  echo "  Cluster nodes:      1/1 Ready"
  echo "  Target node:        $TARGET_NODE"
  echo "  App processes:      $total/$EXPECTED_PROCESSES Ready and local"
  echo "  Debug sidecars:     $sidecars/$EXPECTED_PROCESSES running"
  echo "  DDB binary:         $DDB_BIN"
  echo "  DDB config:         extension.py + runtime-serviceweaver.py"
  echo "  Endpoint:           $endpoint (HTTP 404 = healthy)"
  echo "  Existing DDB:       none"
  echo ""
  echo "Preflight passed."
fi
