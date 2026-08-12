#!/usr/bin/env bash
# Build the recipe-patched SocialNet seeder without changing the source checkout.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

validate_source_inputs
require_command patch

source_root="$SOCIALNET_DIR/src"
source_file="$source_root/bench/init_social_graph.go"
patch_file="$ARTIFACT_DIR/socialnet/init-social-graph-rand.patch"
output_dir="$ARTIFACT_DIR/runtime/socialnet-seeder"
output_bin="$output_dir/init_social.out"

[[ -r "$source_file" ]] || die "SocialNet seeder source not found: $source_file"
[[ -r "$source_root/go.mod" ]] || die "SocialNet Go module not found: $source_root/go.mod"
[[ -r "$source_root/go.sum" ]] || die "SocialNet dependency lockfile not found: $source_root/go.sum"
[[ -r "$patch_file" ]] || die "recipe seeder patch not found: $patch_file"

mkdir -p "$ARTIFACT_DIR/runtime" "$output_dir"
stage="$(mktemp -d "$ARTIFACT_DIR/runtime/socialnet-seeder-build.XXXXXX")"
cleanup() {
  rm -rf "$stage"
}
trap cleanup EXIT

cp "$source_root/go.mod" "$source_root/go.sum" "$stage/"
cp -R "$source_root/shared" "$stage/shared"
mkdir -p "$stage/bench"
cp "$source_file" "$stage/bench/init_social_graph.go"

patch --dry-run --silent -d "$stage" -p1 < "$patch_file" \
  || die "the recipe seeder patch does not match the current SocialNet source"
patch --silent -d "$stage" -p1 < "$patch_file"

if [[ "$(grep -Fc 'r.Intn' "$stage/bench/init_social_graph.go")" -ne 3 ]]; then
  die "patched seeder contains an unexpected direct random-number access"
fi

case "$SOCIALNET_BUILD_MODE" in
docker)
  require_command docker
  docker info >/dev/null 2>&1 \
    || die "cannot access the Docker daemon as $(id -un)"
  note "Building the race-free SocialNet seeder with $SOCIALNET_GO_IMAGE"
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$stage:/src" \
    -v "$output_dir:/out" \
    -w /src \
    -e HOME=/tmp/home \
    -e GOPATH=/tmp/go \
    -e GOCACHE=/tmp/go-cache \
    -e CGO_ENABLED=0 \
    "$SOCIALNET_GO_IMAGE" \
    bash -c 'mkdir -p "$HOME" "$GOPATH" "$GOCACHE"; go build -o /out/init_social.out ./bench'
  ;;
native)
  require_command go
  note "Building the race-free SocialNet seeder with the host Go toolchain"
  (cd "$stage" && CGO_ENABLED=0 go build -o "$output_bin" ./bench)
  ;;
*)
  die "SOCIALNET_BUILD_MODE must be 'docker' or 'native', got '$SOCIALNET_BUILD_MODE'"
  ;;
esac

[[ -x "$output_bin" ]] || die "patched SocialNet seeder build failed: $output_bin"
