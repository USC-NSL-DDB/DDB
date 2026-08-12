#!/usr/bin/env bash
# Deploy this recipe's private gateway, inject sidecars, and render its config.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

check_only=0
case "${1:-}" in
  "") ;;
  --check) check_only=1 ;;
  -h|--help)
    echo "Usage: $0 [--check]"
    exit 0
    ;;
  *) die "unknown option: $1" ;;
esac

require_command kubectl
require_command python3
ensure_kubeconfig
ensure_no_ddb
validate_local_assets

label="$(app_label_value)"
[[ -n "$label" ]] || die "no ServiceWeaver application deployment was found"
selector="$APP_LABEL_KEY=$label"
note "Waiting for exactly $EXPECTED_PROCESSES active application pods"
wait_for_exact_app_pods "$selector"

sidecar_status() {
  k get pods -l "$selector" -o json | python3 -c '
import json, sys

expected = int(sys.argv[1])
items = json.load(sys.stdin)["items"]
running = 0
for pod in items:
    names = {
        item["name"]
        for item in pod.get("spec", {}).get("ephemeralContainers", [])
        if item["name"].startswith("ssh-debugger-")
    }
    statuses = {
        item["name"]: item.get("state", {})
        for item in pod.get("status", {}).get("ephemeralContainerStatuses", [])
    }
    ready = bool(names) and any("running" in statuses.get(name, {}) for name in names)
    marker = "ok" if ready else "--"
    pod_name = pod["metadata"]["name"]
    print(f"  {marker} {pod_name}")
    running += int(ready)
print(f"Debugger sidecars: {running}/{len(items)} running")
raise SystemExit(0 if len(items) == expected and running == expected else 1)
' "$EXPECTED_PROCESSES"
}

if [[ "$check_only" -eq 1 ]]; then
  sidecar_status
  exit $?
fi

pod_count="$(k get pods -l "$selector" --no-headers 2>/dev/null | wc -l)"
[[ "$pod_count" -eq "$EXPECTED_PROCESSES" ]] \
  || die "expected $EXPECTED_PROCESSES application pods, found $pod_count"

note "Applying the recipe-owned ClusterIP SSH gateway"
# A Pod's command is immutable. Recreate this recipe's gateway so the live
# resource exactly matches the checked-in manifest.
k delete pod "$GATEWAY_POD_NAME" --ignore-not-found --wait=true >/dev/null
kubectl --kubeconfig "$KUBECONFIG" -n "$NAMESPACE" apply \
  -f "$GATEWAY_MANIFEST" >/dev/null
kubectl --kubeconfig "$KUBECONFIG" -n "$NAMESPACE" wait \
  --for=condition=Ready "pod/$GATEWAY_POD_NAME" --timeout=180s
gateway_ip="$(k get service "$GATEWAY_SERVICE_NAME" -o jsonpath='{.spec.clusterIP}')"
[[ -n "$gateway_ip" && "$gateway_ip" != "None" ]] \
  || die "could not discover the call-depth SSH gateway ClusterIP"

if ! sidecar_status >/dev/null 2>&1; then
  note "Injecting one debugger sidecar into each application pod"
  python3 "$SIDECAR_INJECTOR" \
    --kubeconfig "$KUBECONFIG" \
    --namespace "$NAMESPACE" \
    --label-key "$APP_LABEL_KEY" \
    --label "$label" \
    --expected "$EXPECTED_PROCESSES"
fi

note "Waiting for all debugger sidecars"
ready=0
for _ in $(seq 1 24); do
  if sidecar_status; then
    ready=1
    break
  fi
  sleep 5
done
[[ "$ready" -eq 1 ]] || die "debugger sidecars did not become ready within 120 seconds"

log_dir="${DDB_LOG_DIR:-$ARTIFACT_DIR/runtime/logs}"
base_dir="${DDB_BASE_DIR:-$ARTIFACT_DIR/runtime}"
mkdir -p "$log_dir" "$base_dir" "$(dirname "$DDB_CONFIG")"

sed \
  -e "s|@SERVICE_NAME@|$label|g" \
  -e "s|@KUBECONFIG_PATH@|$KUBECONFIG|g" \
  -e "s|@GATEWAY_IP@|$gateway_ip|g" \
  -e "s|@LOG_DIR@|$log_dir|g" \
  -e "s|@BASE_DIR@|$base_dir|g" \
  "$DDB_CONFIG_TEMPLATE" > "$DDB_CONFIG"

validate_runtime_config
echo "DDB config: $DDB_CONFIG"
echo "SSH gateway: $gateway_ip:2222 (ClusterIP only)"
