#!/usr/bin/env bash
# Clean call-depth-only placement and debugger resources on the single node.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

if [[ "${1:-}" != "--yes" ]]; then
  cat >&2 <<EOF
This removes the artifact node selector, restarts all application pods, removes
the private SSH gateway, and reseeds the in-memory application. Debugger
sidecars will be removed with the old pods. It does not modify node taints.

Re-run with: $0 --yes
EOF
  exit 1
fi

ensure_kubeconfig
ensure_no_ddb
validate_source_inputs
validate_local_assets
resolve_target_node

note "Removing the artifact node selector"
mapfile -t deployments < <(app_deployments)
for deployment in "${deployments[@]}"; do
  k patch "$deployment" --type json -p \
    '[{"op":"remove","path":"/spec/template/spec/nodeSelector/kubernetes.io~1hostname"}]' \
    >/dev/null 2>&1 || true
  k rollout restart "$deployment" >/dev/null
done

for deployment in "${deployments[@]}"; do
  k rollout status "$deployment" --timeout=300s
done

note "Removing the call-depth SSH gateway"
kubectl --kubeconfig "$KUBECONFIG" -n "$NAMESPACE" delete \
  -f "$GATEWAY_MANIFEST" --ignore-not-found >/dev/null

note "Reseeding the in-memory social graph"
bash "$ARTIFACT_DIR/seed_data.sh" --addr "$(detect_endpoint)"

echo ""
echo "Single-node cleanup complete; SocialNet is running without call-depth debugger resources."
echo "Node taints were not changed because their pre-run state was not recorded."
