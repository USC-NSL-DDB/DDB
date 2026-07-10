#!/bin/bash
#
# Deploy the socialnet app to the k3s cluster.
#
#   weaver-kube deploy src/server/config.yaml   ->  /tmp/kube_<id>.yaml
#   kubectl apply -f /tmp/kube_<id>.yaml
#
# Then waits for pods, patches apilistener to NodePort, and exports the live
# manifests to socialnet-manifests.yaml (used by ./redeploy_app.sh --full).
#
# Usage:
#   ./deploy_app.sh             # no-op if the app is already deployed
#   ./deploy_app.sh --force     # deploy again even if apilistener exists
#
# Note: the container image is taken from src/server/config.yaml. weaver-kube
# builds it locally but does not push it, so worker nodes pull that image tag
# from Docker Hub. If you point config.yaml at your own image, set `repo:` too
# so weaver-kube pushes it where the workers can reach it.

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

FORCE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --force) FORCE=1; shift ;;
    *) die "Unknown option: $1" ;;
  esac
done

ensure_kubeconfig

# Check for an existing deployment before demanding docker/weaver-kube: a no-op
# re-run should not require build tooling it will never use.
if app_is_deployed && [[ "$FORCE" -eq 0 ]]; then
  echo "App already deployed ($(apilistener_svc)). Use --force to redeploy,"
  echo "or ./redeploy_app.sh to restart pods with fresh state."
  patch_nodeport
  exit 0
fi

command -v weaver-kube >/dev/null || die "weaver-kube not found on PATH.
  Install it with:
    go install github.com/ServiceWeaver/weaver-kube/cmd/weaver-kube@v0.23.0"

# weaver-kube builds the container image with `docker build`.
ensure_docker

[[ -f "$SOCIALNET_DIR/src/server/server.out" ]] || die "server.out not found. Run ./build_app.sh first."

echo "=== Generating Kubernetes manifests with weaver-kube ==="
# weaver-kube prints progress on stderr and the generated YAML path as the last
# line of stdout.
GENERATED_YAML="$( cd "$SOCIALNET_DIR" && weaver-kube deploy src/server/config.yaml | tail -n 1 )"

[[ -f "$GENERATED_YAML" ]] || die "weaver-kube did not produce a manifest (got: '$GENERATED_YAML')"
echo "Generated: $GENERATED_YAML"

echo ""
echo "=== Applying to cluster ==="
kubectl apply -f "$GENERATED_YAML"

echo ""
wait_for_pods

echo ""
echo "=== Exposing apilistener via NodePort ==="
patch_nodeport

echo ""
export_manifests

echo ""
echo "=== Pod distribution ==="
kubectl get pods -o wide -n "$NAMESPACE" --sort-by='.spec.nodeName' | grep -v ssh-gateway || true

echo ""
echo "=== Deploy complete ==="
echo "Next: ./seed_data.sh && ./run_benchmark.sh"
