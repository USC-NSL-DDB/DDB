#!/bin/bash
#
# Head-node-only setup: weaver-kube (generates the k8s manifests) and the
# git submodules that hold the socialnetwork app.

set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

command -v go >/dev/null 2>&1 \
  || die "Go not found (looked in /usr/local/go/bin and \$HOME/go/bin).
  Run ./deploy_all.sh first — it installs Go on every node."

echo "Installing weaver-kube..."
go install github.com/ServiceWeaver/weaver-kube/cmd/weaver-kube@v0.23.0
command -v weaver-kube >/dev/null 2>&1 || die "weaver-kube not on PATH after 'go install'"

echo "Checking out submodules..."
git -C "$REPO_ROOT" submodule update --init --recursive --jobs "$(nproc)"
[[ -f "$SOCIALNET_DIR/src/server/config.yaml" ]] \
  || die "socialnetwork submodule missing at $SOCIALNET_DIR"

echo "Head node ready (weaver-kube $(weaver-kube --version 2>/dev/null | head -1 || echo installed))"
