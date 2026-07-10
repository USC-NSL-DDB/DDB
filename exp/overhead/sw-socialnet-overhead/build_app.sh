#!/bin/bash
#
# Build the socialnet binaries (server.out, client.out, init_social.out).
#
# By default the build runs inside a golang:1.21.1 container, which is what the
# DDB docs recommend — the app pins weaver v0.22.0 and needs Go 1.21.1 exactly.
# Use --native to build with the host toolchain instead.
#
# Usage:
#   ./build_app.sh              # docker build (recommended)
#   ./build_app.sh --native     # host build (requires go1.21.1 + weaver on PATH)
#   ./build_app.sh --force      # rebuild even if binaries already exist

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

GO_IMAGE="golang:1.21.1"
MODE="docker"
FORCE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --native) MODE="native"; shift ;;
    --force)  FORCE=1;       shift ;;
    *) die "Unknown option: $1" ;;
  esac
done

[[ -d "$SOCIALNET_DIR" ]] || die "socialnetwork submodule not found at $SOCIALNET_DIR
  Run: git submodule update --init --recursive"

SERVER_BIN="$SOCIALNET_DIR/src/server/server.out"
CLIENT_BIN="$SOCIALNET_DIR/src/client/client.out"
BENCH_BIN="$SOCIALNET_DIR/src/bench/init_social.out"

if [[ "$FORCE" -eq 0 && -f "$SERVER_BIN" && -f "$CLIENT_BIN" && -f "$BENCH_BIN" ]]; then
  echo "Binaries already built. Use --force to rebuild."
  exit 0
fi

if [[ "$MODE" == "docker" ]]; then
  ensure_docker
  echo "=== Building socialnet in $GO_IMAGE ==="
  docker run --rm -v "$SOCIALNET_DIR":/app -w /app -e VERSION="dev" "$GO_IMAGE" bash ./build.sh

  # build.sh links against the container's glibc (bookworm, 2.36). server.out is
  # fine -- it only ever runs inside the image weaver-kube builds. But client.out
  # and init_social.out run on the HOST, which is Ubuntu 20.04 (glibc 2.31), and
  # would die with "GLIBC_2.34 not found". Rebuild those two statically.
  echo "=== Rebuilding host-side binaries statically (CGO_ENABLED=0) ==="
  docker run --rm -v "$SOCIALNET_DIR":/app -w /app -e CGO_ENABLED=0 "$GO_IMAGE" \
    bash -c 'cd src/client && go build -o client.out . && cd ../bench && go build -o init_social.out .'

  # The container writes as root; hand the tree back to the invoking user so the
  # host can run the binaries and later `git submodule` / `go build` still work.
  if [[ -n "$(find "$SOCIALNET_DIR" -user root -print -quit 2>/dev/null)" ]]; then
    echo "Restoring file ownership after container build..."
    sudo chown -R "$(id -u):$(id -g)" "$SOCIALNET_DIR"
  fi
else
  echo "=== Building socialnet with host toolchain ==="
  command -v go >/dev/null || die "go not found on PATH"
  ( cd "$SOCIALNET_DIR" && ./build.sh )
fi

for bin in "$SERVER_BIN" "$CLIENT_BIN" "$BENCH_BIN"; do
  [[ -f "$bin" ]] || die "expected binary not produced: $bin"
  chmod +x "$bin"
done

echo ""
echo "=== Build complete ==="
echo "  $SERVER_BIN"
echo "  $CLIENT_BIN"
echo "  $BENCH_BIN"
