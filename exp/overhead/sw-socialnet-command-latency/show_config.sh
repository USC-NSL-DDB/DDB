#!/usr/bin/env bash
# Print the effective command-latency configuration without changing the cluster.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

display_target="$TARGET_NODE"
if command -v kubectl >/dev/null 2>&1 && [[ -r "$KUBECONFIG" ]]; then
  if ! kubectl --kubeconfig "$KUBECONFIG" get node "$display_target" >/dev/null 2>&1; then
    detected="$(kubectl --kubeconfig "$KUBECONFIG" get nodes \
      -l node-role.kubernetes.io/control-plane \
      -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
    [[ -z "$detected" ]] || display_target="$detected"
  fi
fi
display_service="$K3S_SERVICE"
if [[ -z "$display_service" ]] && detect_native_k3s; then
  display_service="$K3S_SERVICE"
fi
kubectl_path="$(command -v kubectl 2>/dev/null || true)"
weaver_kube_path="$(command -v weaver-kube 2>/dev/null || true)"
worker_count=0
if [[ -r "$WORKERS_FILE" ]]; then
  worker_count="$(awk '{sub(/#.*/, ""); if ($0 ~ /[^[:space:]]/) count++} END {print count + 0}' "$WORKERS_FILE")"
fi
cluster_node_count=$((worker_count + 1))

cat <<EOF
Recipe:                    command latency
Artifact directory:        $ARTIFACT_DIR
DDB repository:            ${DDB_REPO_ROOT:-<not detected>}
DDB Rust source:           $DDB_SOURCE_DIR
DDB binary:                $DDB_BIN
SocialNet source:          $SOCIALNET_DIR
Controller private IP:     ${CONTROLLER_IP:-<set in artifact.env>}
Worker SSH inventory:      $WORKERS_FILE ($worker_count configured)
SSH identity:              ${SSH_IDENTITY_FILE:-<default SSH configuration>}
Allow active firewalld:    $ALLOW_ACTIVE_FIREWALL
Kubeconfig:                $KUBECONFIG
Native-k3s service:        ${display_service:-<no active service detected>}
Native-k3s install pin:    $K3S_INSTALL_VERSION
kubectl:                   ${kubectl_path:-<setup installs $KUBECTL_INSTALL_VERSION>}
weaver-kube:               ${weaver_kube_path:-<setup installs $WEAVER_KUBE_INSTALL_VERSION>}
Namespace:                 $NAMESPACE
Control Kubernetes node:   $display_target
Expected cluster nodes:    $cluster_node_count
Application deployments:   $EXPECTED_DEPLOYMENTS
Replicas per deployment:   $SOCIALNET_REPLICAS
Expected app processes:    $EXPECTED_PROCESSES
Command workers:           $COMMAND_WORKERS
Application label key:     $APP_LABEL_KEY
Debugger prefix:           $DEBUGGER_CONTAINER_PREFIX
SocialNet build image:     $SOCIALNET_GO_IMAGE
SocialNet runtime image:   $SOCIALNET_IMAGE
Generated DDB config:      $DDB_CONFIG
Results directory:         $RESULTS_ROOT
Endpoint override:         ${ADDR:-<auto-detect NodePort>}
EOF
