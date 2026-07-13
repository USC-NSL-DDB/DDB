#!/usr/bin/env bash
# Prepare the application and DDB for the single-machine latency evaluation.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

seed=1
setup_ddb=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-seed) seed=0; shift ;;
    --skip-ddb-setup) setup_ddb=0; shift ;;
    -h|--help)
      echo "Usage: $0 [--skip-seed] [--skip-ddb-setup]"
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

cluster_nodes="$(kubectl --kubeconfig "$KUBECONFIG" get nodes --no-headers | wc -l)"
[[ "$cluster_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "call depth requires a one-node native-k3s cluster; found $cluster_nodes nodes"

note "Incrementally building the selected Rust DDB source"
cargo build --release --manifest-path "$DDB_SOURCE_DIR/Cargo.toml"
validate_ddb_binary

note "Preparing the recipe-owned SocialNet deployment"
allow_single_node_workloads
bash "$ARTIFACT_DIR/prepare_socialnet.sh"

note "Pinning the ServiceWeaver application to $TARGET_NODE"
mapfile -t deployments < <(app_deployments)
[[ "${#deployments[@]}" -eq "$EXPECTED_PROCESSES" ]] \
  || die "expected $EXPECTED_PROCESSES application deployments, found ${#deployments[@]}"

for deployment in "${deployments[@]}"; do
  k patch "$deployment" --type merge -p \
    "{\"spec\":{\"template\":{\"spec\":{\"nodeSelector\":{\"kubernetes.io/hostname\":\"$TARGET_NODE\"}}}}}" \
    >/dev/null
done
note "Restarting all application pods to remove stale ephemeral containers"
for deployment in "${deployments[@]}"; do
  k rollout restart "$deployment" >/dev/null
done
for deployment in "${deployments[@]}"; do
  k rollout status "$deployment" --timeout=300s
done

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
