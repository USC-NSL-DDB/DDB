#!/usr/bin/env bash
# Deploy the local gateway, inject debuggers, and render this recipe's DDB config.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

case "${1:-}" in
  "") ;;
  -h|--help)
    echo "Usage: $0"
    exit 0
    ;;
  *) die "unknown option: $1" ;;
esac

require_command kubectl
require_command python3
ensure_kubeconfig
ensure_no_ddb
validate_local_assets

sidecar_counts() {
  local selector="$1"
  k get pods -l "$selector" -o json | python3 -c '
import json, sys
prefix = sys.argv[1]
pods = json.load(sys.stdin)["items"]
running = sum(
    any(s.get("name", "").startswith(prefix)
        and s.get("state", {}).get("running") is not None
        for s in pod.get("status", {}).get("ephemeralContainerStatuses", []))
    for pod in pods
)
print(len(pods), running)
' "$DEBUGGER_CONTAINER_PREFIX"
}

label="$(app_label_value)"
[[ -n "$label" ]] || die "no ServiceWeaver application deployment was found"
selector="$APP_LABEL_KEY=$label"
note "Waiting for exactly $EXPECTED_PROCESSES active application pods"
wait_for_exact_app_pods "$selector"

note "Applying this recipe's internal SSH gateway"
k delete pod "$GATEWAY_NAME" --ignore-not-found --wait=true >/dev/null
k apply -f "$GATEWAY_MANIFEST" >/dev/null
k wait --for=condition=Ready "pod/$GATEWAY_NAME" --timeout=180s >/dev/null
gateway_ip="$(k get service "$GATEWAY_NAME" -o jsonpath='{.spec.clusterIP}')"
[[ -n "$gateway_ip" ]] || die "could not determine $GATEWAY_NAME ClusterIP"

read -r pod_count running_count <<<"$(sidecar_counts "$selector")"
[[ "$pod_count" -eq "$EXPECTED_PROCESSES" ]] \
  || die "expected $EXPECTED_PROCESSES app pods, found $pod_count"
if [[ "$running_count" -ne "$EXPECTED_PROCESSES" ]]; then
  note "Injecting debugger sidecars from this recipe's pinned definition"
  python3 "$SIDECAR_INJECTOR" \
    --kubeconfig "$KUBECONFIG" \
    --namespace "$NAMESPACE" \
    --label-key "$APP_LABEL_KEY" \
    --label-value "$label" \
    --debugger-prefix "$DEBUGGER_CONTAINER_PREFIX" \
    --expected "$EXPECTED_PROCESSES"

  for attempt in $(seq 1 24); do
    read -r pod_count running_count <<<"$(sidecar_counts "$selector")"
    [[ "$running_count" -eq "$EXPECTED_PROCESSES" ]] && break
    sleep 5
  done
  [[ "$running_count" -eq "$EXPECTED_PROCESSES" ]] \
    || die "only $running_count/$EXPECTED_PROCESSES debugger sidecars became ready"
fi

log_dir="${DDB_LOG_DIR:-$ARTIFACT_DIR/runtime/logs}"
base_dir="${DDB_BASE_DIR:-$ARTIFACT_DIR/runtime}"
mkdir -p "$log_dir" "$base_dir" "$(dirname "$DDB_CONFIG")"
python3 - "$DDB_CONFIG_TEMPLATE" "$DDB_CONFIG" \
  "$label" "$KUBECONFIG" "$gateway_ip" "$log_dir" "$base_dir" <<'PY'
from pathlib import Path
import sys

template, output, service, kubeconfig, gateway, logs, base = sys.argv[1:]
text = Path(template).read_text()
for key, value in {
    "@SERVICE_NAME@": service,
    "@KUBECONFIG_PATH@": kubeconfig,
    "@GATEWAY_IP@": gateway,
    "@LOG_DIR@": logs,
    "@BASE_DIR@": base,
}.items():
    text = text.replace(key, value)
if "@" in "".join(line for line in text.splitlines() if not line.lstrip().startswith("#")):
    raise SystemExit("unrendered placeholder remains in DDB config")
Path(output).write_text(text)
PY

note "Wrote local DDB config: $DDB_CONFIG"
