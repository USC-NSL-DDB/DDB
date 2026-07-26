#!/usr/bin/env bash
# Print the effective call-depth configuration without changing the cluster.
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
if [[ -z "$display_service" ]]; then
  display_service="$(systemctl list-units --type=service --state=running \
    --no-legend --plain 2>/dev/null \
    | awk '$1 ~ /^k3s.*\.service$/ { print $1; exit }')"
fi
kubectl_path="$(command -v kubectl 2>/dev/null || true)"
weaver_kube_path="$(command -v weaver-kube 2>/dev/null || true)"

cat <<EOF
Recipe:                    single-node call depth
Artifact directory:        $ARTIFACT_DIR
DDB repository:            ${DDB_REPO_ROOT:-<not detected>}
DDB Rust source:           $DDB_SOURCE_DIR
DDB binary:                $DDB_BIN
SocialNet source:          $SOCIALNET_DIR
Expected SocialNet commit: $EXPECTED_SOCIALNET_COMMIT
Kubeconfig:                $KUBECONFIG
Native-k3s service:        ${display_service:-<no active service detected>}
Native-k3s install pin:    $K3S_INSTALL_VERSION
kubectl:                   ${kubectl_path:-<setup installs $KUBECTL_INSTALL_VERSION>}
weaver-kube:               ${weaver_kube_path:-<setup installs $WEAVER_KUBE_INSTALL_VERSION>}
Namespace:                 $NAMESPACE
Target Kubernetes node:    $display_target
Expected app deployments:  $EXPECTED_DEPLOYMENTS
Expected app processes:    $EXPECTED_PROCESSES
Deployment topology:       all app processes on the target node
Application label key:     $APP_LABEL_KEY
SocialNet build mode:      $SOCIALNET_BUILD_MODE
SocialNet build image:     $SOCIALNET_GO_IMAGE
SocialNet runtime image:   $SOCIALNET_IMAGE
Generated DDB config:      $DDB_CONFIG
Results directory:         $RESULTS_ROOT
Endpoint override:         ${ADDR:-<auto-detect NodePort>}
EOF
