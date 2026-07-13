#!/usr/bin/env bash
# Build and deploy SocialNet using only this recipe's deployment configuration.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

case "${1:-}" in
  "") ;;
  -h|--help)
    echo "Usage: $0"
    echo "Build and deploy SocialNet only when the one-node cluster does not already contain it."
    exit 0
    ;;
  *) die "unknown option: $1" ;;
esac

require_command kubectl
require_command python3
ensure_kubeconfig
validate_source_inputs
validate_local_assets
resolve_target_node

ensure_native_k3s
cluster_nodes="$(kubectl --kubeconfig "$KUBECONFIG" get nodes --no-headers | wc -l)"
[[ "$cluster_nodes" -eq "$EXPECTED_CLUSTER_NODES" ]] \
  || die "SocialNet preparation requires a one-node native-k3s cluster; found $cluster_nodes nodes"

deployment_count="$(app_deployment_count)"
listener="$(apilistener_service)"
app_present=0
if [[ "$deployment_count" -eq "$EXPECTED_PROCESSES" && -n "$listener" ]]; then
  ensure_apilistener_nodeport
  app_present=1
elif [[ "$deployment_count" -ne 0 || -n "$listener" ]]; then
  die "partial SocialNet deployment detected ($deployment_count deployments, listener=${listener:-missing}).
  Remove the partial deployment before re-running setup."
fi

server_bin="$SOCIALNET_DIR/src/server/server.out"
client_bin="$SOCIALNET_DIR/src/client/client.out"
seed_bin="$SOCIALNET_DIR/src/bench/init_social.out"
[[ -f "$SOCIALNET_DIR/build.sh" ]] \
  || die "SocialNet build entry point not found: $SOCIALNET_DIR/build.sh"

needs_build=0
for binary in "$server_bin" "$client_bin" "$seed_bin"; do
  [[ -x "$binary" ]] || needs_build=1
done

if [[ "$app_present" -eq 0 || "$needs_build" -eq 1 ]]; then
  case "$SOCIALNET_BUILD_MODE" in
  docker)
    require_command docker
    docker info >/dev/null 2>&1 \
      || die "cannot access the Docker daemon as $(id -un)"
    note "Building the accepted SocialNet source with $SOCIALNET_GO_IMAGE"
    docker run --rm \
      --user "$(id -u):$(id -g)" \
      -v "$SOCIALNET_DIR:/app" \
      -w /app \
      -e HOME=/tmp/home \
      -e GOPATH=/tmp/go \
      -e GOCACHE=/tmp/go-cache \
      -e VERSION=dev \
      "$SOCIALNET_GO_IMAGE" \
      bash -c 'mkdir -p "$HOME" "$GOPATH" "$GOCACHE"; export PATH="$GOPATH/bin:$PATH"; bash ./build.sh'

    # These two binaries execute on the host, so keep them independent of the
    # Go container's glibc version.
    docker run --rm \
      --user "$(id -u):$(id -g)" \
      -v "$SOCIALNET_DIR:/app" \
      -w /app \
      -e HOME=/tmp/home \
      -e GOPATH=/tmp/go \
      -e GOCACHE=/tmp/go-cache \
      -e CGO_ENABLED=0 \
      "$SOCIALNET_GO_IMAGE" \
      bash -c 'mkdir -p "$HOME" "$GOPATH" "$GOCACHE"; cd src/client && go build -o client.out .; cd ../bench && go build -o init_social.out .'
      ;;
  native)
    require_command go
    note "Building the accepted SocialNet source with the host Go toolchain"
    (cd "$SOCIALNET_DIR" && bash ./build.sh)
      ;;
  *)
    die "SOCIALNET_BUILD_MODE must be 'docker' or 'native', got '$SOCIALNET_BUILD_MODE'"
      ;;
  esac
fi

for binary in "$server_bin" "$client_bin" "$seed_bin"; do
  [[ -f "$binary" ]] || die "SocialNet build did not produce $binary"
  chmod +x "$binary"
done

if [[ "$app_present" -eq 1 ]]; then
  note "SocialNet is already deployed ($deployment_count application processes); keeping it"
  exit 0
fi

require_command docker
require_command weaver-kube
require_command sudo
[[ -n "$K3S_BIN" && -x "$K3S_BIN" ]] \
  || die "native k3s executable was not resolved from $K3S_SERVICE"
docker info >/dev/null 2>&1 \
  || die "cannot access the Docker daemon as $(id -un)"

allow_single_node_workloads
kubectl --kubeconfig "$KUBECONFIG" get namespace "$NAMESPACE" >/dev/null 2>&1 \
  || kubectl --kubeconfig "$KUBECONFIG" create namespace "$NAMESPACE" >/dev/null

runtime_dir="$ARTIFACT_DIR/runtime/socialnet"
mkdir -p "$runtime_dir"
weaver_config="$runtime_dir/weaver.toml"
kube_config="$runtime_dir/config.yaml"

escape_sed_replacement() {
  printf '%s' "$1" | sed 's/[\/&]/\\&/g'
}

server_escaped="$(escape_sed_replacement "$server_bin")"
weaver_escaped="$(escape_sed_replacement "$weaver_config")"
image_escaped="$(escape_sed_replacement "$SOCIALNET_IMAGE")"
namespace_escaped="$(escape_sed_replacement "$NAMESPACE")"
sed "s/@SERVER_BIN@/$server_escaped/g" \
  "$SOCIALNET_WEAVER_TEMPLATE" > "$weaver_config"
sed \
  -e "s/@APP_CONFIG@/$weaver_escaped/g" \
  -e "s/@SOCIALNET_IMAGE@/$image_escaped/g" \
  -e "s/@NAMESPACE@/$namespace_escaped/g" \
  "$SOCIALNET_CONFIG_TEMPLATE" > "$kube_config"

note "Generating SocialNet manifests from this recipe's configuration"
generated_yaml="$(weaver-kube deploy "$kube_config" | tail -n 1)"
[[ -f "$generated_yaml" ]] \
  || die "weaver-kube did not produce a manifest (reported '$generated_yaml')"
cp "$generated_yaml" "$runtime_dir/manifests.yaml"

# weaver-kube builds with Docker while native k3s normally uses containerd.
# Import the exact image built from this source so k3s cannot silently pull a
# different image with the same tag from a registry.
image_archive="$runtime_dir/socialnet-image.tar"
trap 'rm -f "$image_archive"' EXIT
note "Importing the freshly built SocialNet image into native k3s"
docker image inspect "$SOCIALNET_IMAGE" >/dev/null \
  || die "weaver-kube did not build the configured image: $SOCIALNET_IMAGE"
docker save --output "$image_archive" "$SOCIALNET_IMAGE"
sudo env K3S_DATA_DIR="$K3S_DATA_DIR" \
  "$K3S_BIN" ctr images import "$image_archive" >/dev/null

note "Applying the generated SocialNet manifests"
kubectl --kubeconfig "$KUBECONFIG" apply -f "$runtime_dir/manifests.yaml" >/dev/null

for _ in $(seq 1 30); do
  [[ "$(app_deployment_count)" -eq "$EXPECTED_PROCESSES" ]] && break
  sleep 2
done
mapfile -t deployments < <(app_deployments)
[[ "${#deployments[@]}" -eq "$EXPECTED_PROCESSES" ]] \
  || die "expected $EXPECTED_PROCESSES SocialNet deployments, found ${#deployments[@]}"
for deployment in "${deployments[@]}"; do
  k rollout status "$deployment" --timeout=300s
done

ensure_apilistener_nodeport
echo "SocialNet endpoint: $(detect_endpoint)"
