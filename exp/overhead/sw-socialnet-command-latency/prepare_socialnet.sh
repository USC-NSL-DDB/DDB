#!/usr/bin/env bash
# Build and deploy SocialNet with the shared artifact runtime image.
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

[[ "$skip_build" -eq 0 || "$force_build" -eq 0 ]] \
  || die "--skip-build and --force-build cannot be used together"

require_command docker
require_command sudo
docker info >/dev/null 2>&1 \
  || die "cannot access the Docker daemon as $(whoami)"

deployment_count="$(k get deployments -l "$APP_LABEL_KEY" -o json 2>/dev/null | python3 -c '
import json, sys
print(len(json.load(sys.stdin).get("items", [])))
' || echo 0)"
if [[ "$deployment_count" -eq 0 ]]; then
  [[ "$skip_deploy" -eq 0 ]] \
    || die "SocialNet is not deployed and --skip-deploy was requested"
elif [[ "$deployment_count" -ne "$EXPECTED_DEPLOYMENTS" ]]; then
  die "found a partial SocialNet deployment: $deployment_count/$EXPECTED_DEPLOYMENTS deployments"
fi

server_bin="$SOCIALNET_DIR/src/server/server.out"
client_bin="$SOCIALNET_DIR/src/client/client.out"
seed_bin="$SOCIALNET_DIR/src/bench/init_social.out"

image_present=0
if docker image inspect "$SOCIALNET_IMAGE" >/dev/null 2>&1; then
  image_present=1
fi

needs_source_build=$force_build
[[ "$image_present" -eq 1 ]] || needs_source_build=1
[[ "$deployment_count" -ne 0 ]] || needs_source_build=1
for binary in "$server_bin" "$client_bin" "$seed_bin"; do
  [[ -x "$binary" ]] || needs_source_build=1
done

if [[ "$needs_source_build" -eq 1 && "$skip_build" -eq 1 ]]; then
  die "--skip-build cannot be used because the shared SocialNet image or its host binaries must be rebuilt"
fi

if [[ "$needs_source_build" -eq 1 ]]; then
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

deploy_config_dir="$ARTIFACT_DIR/runtime/socialnet-deploy"
mkdir -p "$deploy_config_dir"
python3 - "$SOCIALNET_CONFIG_TEMPLATE" "$deploy_config_dir/config.yaml" \
  "$SOCIALNET_APP_TEMPLATE" "$deploy_config_dir/weaver.toml" \
  "$server_bin" "$NAMESPACE" "$SOCIALNET_REPLICAS" "$SOCIALNET_IMAGE" <<'PY'
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
    image,
) = sys.argv[1:]
Path(config_output).write_text(
    Path(config_template)
    .read_text()
    .replace("@NAMESPACE@", namespace)
    .replace("@SOCIALNET_REPLICAS@", replicas)
    .replace("@SOCIALNET_IMAGE@", image)
)
Path(app_output).write_text(
    Path(app_template).read_text().replace(
        "@SERVER_BINARY@", str(Path(binary).resolve())
    )
)
PY

if [[ "$deployment_count" -eq 0 || "$force_build" -eq 1 || "$image_present" -eq 0 ]]; then
  require_command weaver-kube
  note "Building the shared SocialNet :latest image from the current source"
  generated="$(weaver-kube deploy "$deploy_config_dir/config.yaml" | tail -n 1)"
  [[ -r "$generated" ]] || die "weaver-kube did not return a readable manifest: $generated"
  cp "$generated" "$deploy_config_dir/manifests.yaml"
  sed -i -E \
    's/^([[:space:]]*)imagePullPolicy: (Always|IfNotPresent)$/\1imagePullPolicy: Never/' \
    "$deploy_config_dir/manifests.yaml"
else
  note "Using the shared SocialNet runtime image $SOCIALNET_IMAGE"
fi

docker image inspect "$SOCIALNET_IMAGE" >/dev/null 2>&1 \
  || die "weaver-kube did not build the configured image: $SOCIALNET_IMAGE"
[[ -n "$K3S_BIN" && -x "$K3S_BIN" ]] \
  || die "native k3s executable was not resolved from $K3S_SERVICE"

image_archive="$deploy_config_dir/socialnet-image.tar"
trap 'rm -f "$image_archive"' EXIT
docker save --output "$image_archive" "$SOCIALNET_IMAGE"
note "Importing the shared SocialNet image into the controller"
sudo env K3S_DATA_DIR="$K3S_DATA_DIR" \
  "$K3S_BIN" ctr images import "$image_archive" >/dev/null

load_worker_targets
for target in "${WORKER_TARGETS[@]}"; do
  note "Importing the shared SocialNet image into $target"
  worker_ssh "$target" sudo env K3S_DATA_DIR=/var/lib/rancher/k3s \
    /usr/local/bin/k3s ctr images import - < "$image_archive" >/dev/null \
    || die "failed to import the SocialNet image into $target"
done

if [[ "$deployment_count" -eq 0 ]]; then
  [[ -r "$deploy_config_dir/manifests.yaml" ]] \
    || die "generated SocialNet manifest is missing: $deploy_config_dir/manifests.yaml"
  note "Applying the generated SocialNet resources"
  k apply -f "$deploy_config_dir/manifests.yaml"
  k wait --for=condition=Available deployment -l "$APP_LABEL_KEY" --timeout=300s
fi

deployment_count="$(k get deployments -l "$APP_LABEL_KEY" -o name | wc -l)"
[[ "$deployment_count" -eq "$EXPECTED_DEPLOYMENTS" ]] \
  || die "expected $EXPECTED_DEPLOYMENTS SocialNet deployments, found $deployment_count"

listener="$(apilistener_service)"
[[ -n "$listener" ]] || die "SocialNet apilistener service was not created"
k patch "$listener" --type merge -p '{"spec":{"type":"NodePort"}}' >/dev/null
note "SocialNet application is deployed: $(detect_endpoint)"
