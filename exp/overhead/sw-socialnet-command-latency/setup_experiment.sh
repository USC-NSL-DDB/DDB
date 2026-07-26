#!/usr/bin/env bash
# Prepare the complete multi-node cluster for command-latency evaluation.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

seed=1
setup_ddb=1
app_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-seed) seed=0; shift ;;
    --skip-ddb-setup) setup_ddb=0; shift ;;
    --skip-app-build) app_args+=(--skip-build); shift ;;
    --skip-app-deploy) app_args+=(--skip-deploy); shift ;;
    --force-app-build) app_args+=(--force-build); shift ;;
    -h|--help)
      echo "Usage: $0 [--skip-seed] [--skip-ddb-setup] [--skip-app-build] [--skip-app-deploy] [--force-app-build]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

require_command python3
require_command curl
require_command patch
require_command awk
require_command ip
require_command docker
require_command cargo
require_command go
require_command git
require_command ssh
require_command sudo
validate_cluster_inputs
ensure_no_ddb
validate_source_inputs
validate_local_assets
docker info >/dev/null 2>&1 \
  || die "cannot access the Docker daemon as $(id -un)"

note "Bootstrapping the controller and $EXPECTED_WORKERS worker node(s)"
"$ARTIFACT_DIR/bootstrap_cluster.sh"

require_command kubectl
require_command weaver-kube
ensure_kubeconfig
ensure_native_k3s
resolve_target_node

[[ -x "$ARTIFACT_DIR/seed_data.sh" ]] || die "local seed_data.sh is not executable"
[[ -x "$ARTIFACT_DIR/setup_ddb.sh" ]] || die "local setup_ddb.sh is not executable"

read -r cluster_nodes ready_nodes <<<"$(kubectl --kubeconfig "$KUBECONFIG" get nodes -o json | python3 -c '
import json, sys
items = json.load(sys.stdin)["items"]
ready = sum(any(c["type"] == "Ready" and c["status"] == "True" for c in n["status"]["conditions"]) for n in items)
print(len(items), ready)
')"
[[ "$cluster_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "expected $EXPECTED_CLUSTER_NODES k3s nodes before setup, found $cluster_nodes"
[[ "$ready_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "only $ready_nodes/$EXPECTED_CLUSTER_NODES k3s nodes are Ready before setup"

note "Building the selected Rust DDB source (incremental)"
cargo build --release --manifest-path "$DDB_SOURCE_DIR/Cargo.toml"
validate_ddb_binary

kubectl --kubeconfig "$KUBECONFIG" taint node "$TARGET_NODE" \
  node-role.kubernetes.io/control-plane=:NoSchedule --overwrite >/dev/null

"$ARTIFACT_DIR/prepare_socialnet.sh" "${app_args[@]}"

mapfile -t deployments < <(app_deployments)
[[ "${#deployments[@]}" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS application deployments, found ${#deployments[@]}"

note "Configuring application placement and replicas"
label_value="$(app_label_value)"
mapfile -t autoscalers < <(k get horizontalpodautoscalers \
  -l "$APP_LABEL_KEY=$label_value" -o name)
[[ "${#autoscalers[@]}" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS SocialNet autoscalers, found ${#autoscalers[@]}"
replica_patch="$(printf '{"spec":{"minReplicas":%s,"maxReplicas":%s}}' \
  "$SOCIALNET_REPLICAS" "$SOCIALNET_REPLICAS")"
for autoscaler in "${autoscalers[@]}"; do
  k patch "$autoscaler" --type merge -p "$replica_patch" >/dev/null
done

note "Scaling $EXPECTED_DEPLOYMENTS deployments to $SOCIALNET_REPLICAS replica(s) each"
spread_patch="$(python3 - "$APP_LABEL_KEY" "$label_value" <<'PY'
import json, sys
key, value = sys.argv[1:]
print(json.dumps({"spec": {"template": {"spec": {
    "nodeSelector": {"ddb-artifact-role": "worker"},
    "topologySpreadConstraints": [{
        "maxSkew": 1,
        "topologyKey": "kubernetes.io/hostname",
        "whenUnsatisfiable": "DoNotSchedule",
        "labelSelector": {"matchLabels": {key: value}},
    }],
}}}}))
PY
)"
for deployment in "${deployments[@]}"; do
  k set image "$deployment" "serviceweaver=$SOCIALNET_RUNTIME_IMAGE" >/dev/null
  k patch "$deployment" --type json -p \
    '[{"op":"remove","path":"/spec/template/spec/nodeSelector"}]' \
    >/dev/null 2>&1 || true
  k patch "$deployment" --type merge -p "$spread_patch" >/dev/null
  k scale "$deployment" --replicas="$SOCIALNET_REPLICAS" >/dev/null
  k rollout restart "$deployment" >/dev/null
done

for deployment in "${deployments[@]}"; do
  k rollout status "$deployment" --timeout=300s
done

note "Removing debugger resources from the call-depth recipe"
k delete pod,service -l ddb-artifact=sw-socialnet-call-depth \
  --ignore-not-found >/dev/null

if [[ "$seed" -eq 1 ]]; then
  note "Seeding the distributed application"
  "$ARTIFACT_DIR/seed_data.sh" --addr "$(detect_endpoint)"
fi

if [[ "$setup_ddb" -eq 1 ]]; then
  note "Preparing DDB from this command-latency recipe"
  "$ARTIFACT_DIR/setup_ddb.sh"
fi

validate_ddb_binary
mkdir -p "$RESULTS_ROOT"

if [[ "$setup_ddb" -eq 1 ]]; then
  validate_runtime_config
  note "Running the command-latency preflight"
  "$ARTIFACT_DIR/check_cluster.sh"
else
  note "Skipped DDB setup and preflight as requested"
fi

echo ""
echo "Setup complete. Next run:"
echo "  ./artifact.sh smoke"
