#!/usr/bin/env bash
# Build and deploy the accepted ServiceWeaver SocialNet source without another recipe.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

skip_build=0
skip_deploy=0
force_build=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) skip_build=1; shift ;;
    --skip-deploy) skip_deploy=1; shift ;;
    --force-build) force_build=1; shift ;;
    -h|--help)
      echo "Usage: $0 [--skip-build] [--skip-deploy] [--force-build]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

validate_source_inputs
ensure_kubeconfig
ensure_native_k3s
kubectl --kubeconfig "$KUBECONFIG" get namespace "$NAMESPACE" >/dev/null 2>&1 \
  || kubectl --kubeconfig "$KUBECONFIG" create namespace "$NAMESPACE" >/dev/null

server_bin="$SOCIALNET_DIR/src/server/server.out"
client_bin="$SOCIALNET_DIR/src/client/client.out"
seed_bin="$SOCIALNET_DIR/src/bench/init_social.out"

ensure_docker() {
  require_command docker
  docker info >/dev/null 2>&1 || die "cannot access the Docker daemon as $(whoami)"
}

if [[ "$skip_build" -eq 0 ]] && {
  [[ "$force_build" -eq 1 ]] || [[ ! -x "$server_bin" ]] ||
    [[ ! -x "$client_bin" ]] || [[ ! -x "$seed_bin" ]];
}; then
  ensure_docker
  note "Building ServiceWeaver SocialNet with $SOCIALNET_GO_IMAGE"
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$SOCIALNET_DIR:/app" -w /app \
    -e HOME=/tmp/home -e GOPATH=/tmp/go -e GOCACHE=/tmp/go-cache -e VERSION=dev \
    "$SOCIALNET_GO_IMAGE" \
    bash -c 'mkdir -p "$HOME" "$GOPATH" "$GOCACHE"; export PATH="$GOPATH/bin:$PATH"; bash ./build.sh'
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$SOCIALNET_DIR:/app" -w /app \
    -e HOME=/tmp/home -e GOPATH=/tmp/go -e GOCACHE=/tmp/go-cache -e CGO_ENABLED=0 \
    "$SOCIALNET_GO_IMAGE" bash -c \
    'mkdir -p "$HOME" "$GOPATH" "$GOCACHE"; cd src/client && go build -o client.out .; cd ../bench && go build -o init_social.out .'
fi

for binary in "$server_bin" "$client_bin" "$seed_bin"; do
  [[ -f "$binary" ]] || die "required SocialNet binary is missing: $binary"
  chmod +x "$binary"
done

deployment_count="$(k get deployments -l "$APP_LABEL_KEY" -o json 2>/dev/null | python3 -c '
import json, sys
print(len(json.load(sys.stdin).get("items", [])))
' || echo 0)"
if [[ "$deployment_count" -eq 0 ]]; then
  [[ "$skip_deploy" -eq 0 ]] || die "SocialNet is not deployed and --skip-deploy was requested"
  require_command weaver-kube
  ensure_docker
  deploy_config_dir="$ARTIFACT_DIR/runtime/socialnet-deploy"
  mkdir -p "$deploy_config_dir"
  python3 - "$SOCIALNET_CONFIG_TEMPLATE" "$deploy_config_dir/config.yaml" \
    "$SOCIALNET_APP_TEMPLATE" "$deploy_config_dir/weaver.toml" \
    "$server_bin" "$NAMESPACE" "$SOCIALNET_REPLICAS" <<'PY'
from pathlib import Path
import sys

(
    config_template,
    config_output,
    app_template,
    app_output,
    binary,
    namespace,
    replicas,
) = sys.argv[1:]
Path(config_output).write_text(
    Path(config_template)
    .read_text()
    .replace("@NAMESPACE@", namespace)
    .replace("@SOCIALNET_REPLICAS@", replicas)
)
Path(app_output).write_text(
    Path(app_template).read_text().replace(
        "@SERVER_BINARY@", str(Path(binary).resolve())
    )
)
PY
  note "Generating SocialNet Kubernetes resources from this recipe's config"
  generated="$(weaver-kube deploy "$deploy_config_dir/config.yaml" | tail -n 1)"
  [[ -r "$generated" ]] || die "weaver-kube did not return a readable manifest: $generated"
  k apply -f "$generated"
  k wait --for=condition=Available deployment -l "$APP_LABEL_KEY" --timeout=300s
elif [[ "$deployment_count" -ne "$EXPECTED_DEPLOYMENTS" ]]; then
  die "found a partial SocialNet deployment: $deployment_count/$EXPECTED_DEPLOYMENTS deployments"
fi

deployment_count="$(k get deployments -l "$APP_LABEL_KEY" -o name | wc -l)"
[[ "$deployment_count" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS SocialNet deployments, found $deployment_count"

listener="$(apilistener_service)"
[[ -n "$listener" ]] || die "SocialNet apilistener service was not created"
k patch "$listener" --type merge -p '{"spec":{"type":"NodePort"}}' >/dev/null
note "SocialNet application is deployed: $(detect_endpoint)"
