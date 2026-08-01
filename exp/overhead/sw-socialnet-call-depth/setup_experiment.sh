#!/usr/bin/env bash
# Prepare the application and DDB for the single-node latency evaluation.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

seed=1
setup_ddb=1
rebuild_app=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-seed) seed=0; shift ;;
    --skip-ddb-setup) setup_ddb=0; shift ;;
    --rebuild-app) rebuild_app=1; shift ;;
    -h|--help)
      echo "Usage: $0 [--skip-seed] [--skip-ddb-setup] [--rebuild-app]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

require_command python3
require_command curl
require_command patch
require_command docker
require_command cargo
require_command go
require_command git
require_command sudo

ensure_no_ddb
validate_source_inputs
validate_local_assets
docker info >/dev/null 2>&1 \
  || die "cannot access the Docker daemon as $(id -un)"

[[ -r "$ARTIFACT_DIR/bootstrap_tools.sh" ]] \
  || die "recipe bootstrap is missing: $ARTIFACT_DIR/bootstrap_tools.sh"
# shellcheck disable=SC1091
source "$ARTIFACT_DIR/bootstrap_tools.sh"
bootstrap_runtime_tools

resolve_target_node
ensure_native_k3s

target_ready="$(kubectl --kubeconfig "$KUBECONFIG" get node "$TARGET_NODE" \
  -o jsonpath='{range .status.conditions[?(@.type=="Ready")]}{.status}{end}')"
[[ "$target_ready" == "True" ]] \
  || die "target node $TARGET_NODE is not Ready"

note "Incrementally building the selected Rust DDB source"
cargo build --release --manifest-path "$DDB_SOURCE_DIR/Cargo.toml"
validate_ddb_binary

note "Preparing the recipe-owned SocialNet deployment"
allow_target_node_workloads
prepare_args=()
[[ "$rebuild_app" -eq 0 ]] || prepare_args+=(--rebuild)
bash "$ARTIFACT_DIR/prepare_socialnet.sh" "${prepare_args[@]}"

mapfile -t deployments < <(app_deployments)
[[ "${#deployments[@]}" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS application deployments, found ${#deployments[@]}"

label_value="$(app_label_value)"
mapfile -t autoscalers < <(k get horizontalpodautoscalers \
  -l "$APP_LABEL_KEY=$label_value" -o name)
[[ "${#autoscalers[@]}" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS SocialNet autoscalers, found ${#autoscalers[@]}"

note "Fixing each application process at one replica"
for autoscaler in "${autoscalers[@]}"; do
  k patch "$autoscaler" --type merge \
    -p '{"spec":{"minReplicas":1,"maxReplicas":1}}' >/dev/null
done

placement_patch="$(python3 - "$TARGET_NODE" <<'PY'
import json
import sys

print(json.dumps({
    "spec": {
        "template": {
            "spec": {
                "nodeSelector": {
                    "kubernetes.io/hostname": sys.argv[1],
                },
            },
        },
    },
}))
PY
)"
note "Pinning all $EXPECTED_PROCESSES application processes to $TARGET_NODE"
for deployment in "${deployments[@]}"; do
  k set image "$deployment" "serviceweaver=$SOCIALNET_IMAGE" >/dev/null
  k patch "$deployment" -p \
    '{"spec":{"template":{"spec":{"containers":[{"name":"serviceweaver","imagePullPolicy":"Never"}]}}}}' \
    >/dev/null
  # Replace any multi-node recipe selector instead of merging incompatible
  # placement requirements into it.
  k patch "$deployment" --type json -p \
    '[{"op":"remove","path":"/spec/template/spec/nodeSelector"}]' \
    >/dev/null 2>&1 || true
  k patch "$deployment" --type json -p \
    '[{"op":"remove","path":"/spec/template/spec/topologySpreadConstraints"}]' \
    >/dev/null 2>&1 || true
  k patch "$deployment" --type merge -p "$placement_patch" >/dev/null
  k scale "$deployment" --replicas=1 >/dev/null
done
note "Restarting all application pods to remove stale ephemeral containers"
for deployment in "${deployments[@]}"; do
  k rollout restart "$deployment" >/dev/null
done
for deployment in "${deployments[@]}"; do
  k rollout status "$deployment" --timeout=300s
done

note "Removing debugger resources from the command-latency recipe"
k delete pod,service -l ddb-artifact=sw-socialnet-command-latency \
  --ignore-not-found >/dev/null

if [[ "$seed" -eq 1 ]]; then
  note "Seeding the in-memory social graph"
  bash "$ARTIFACT_DIR/seed_data.sh" --addr "$(detect_endpoint)"
fi

if [[ "$setup_ddb" -eq 1 ]]; then
  note "Injecting debugger sidecars and rendering the DDB config"
  bash "$ARTIFACT_DIR/setup_ddb.sh"
fi

mkdir -p "$RESULTS_ROOT"

if [[ "$setup_ddb" -eq 1 ]]; then
  validate_runtime_config
  note "Running the artifact preflight"
  "$ARTIFACT_DIR/check_cluster.sh"
else
  note "Skipped DDB setup and full preflight as requested"
fi

echo ""
echo "Setup complete. Next run:"
echo "  ./artifact.sh smoke"
