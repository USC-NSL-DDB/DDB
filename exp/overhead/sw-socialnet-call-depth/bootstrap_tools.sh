#!/usr/bin/env bash
# Install only the Kubernetes tools not assumed by this artifact recipe.

select_native_kubeconfig() {
  if [[ "$KUBECONFIG_EXPLICIT" -eq 1 ]]; then
    return 0
  fi

  local service_config target
  service_config="$(sed -n 's/.*--write-kubeconfig=\([^ ;}]*\).*/\1/p' \
    <<<"$K3S_EXEC_START")"
  service_config="${service_config:-/etc/rancher/k3s/k3s.yaml}"

  if [[ -r "$service_config" ]]; then
    KUBECONFIG="$service_config"
  elif sudo test -r "$service_config"; then
    target="$HOME/.kube/sw-socialnet-call-depth-k3s.yaml"
    mkdir -p "$HOME/.kube"
    sudo install -m 0600 -o "$(id -u)" -g "$(id -g)" \
      "$service_config" "$target"
    KUBECONFIG="$target"
  else
    die "the kubeconfig written by $K3S_SERVICE is not readable: $service_config
  Set KUBECONFIG in $ARTIFACT_DIR/artifact.env for this existing service."
  fi
  export KUBECONFIG
}

bootstrap_install_k3s() {
  if detect_native_k3s; then
    select_native_kubeconfig
    note "Using native k3s service $K3S_SERVICE"
    return 0
  fi

  require_command sudo
  require_command curl
  require_command systemctl

  local work installer
  work="$(mktemp -d)"
  installer="$work/install-k3s.sh"

  note "Installing native k3s $K3S_INSTALL_VERSION"
  curl -fsSL https://get.k3s.io -o "$installer"
  sudo env \
    INSTALL_K3S_VERSION="$K3S_INSTALL_VERSION" \
    INSTALL_K3S_EXEC="server --write-kubeconfig-mode=644 --disable=traefik --disable=servicelb --disable=metrics-server --disable=local-storage" \
    sh "$installer"
  rm -rf "$work"

  K3S_SERVICE=k3s.service
  K3S_BIN=/usr/local/bin/k3s
  K3S_DATA_DIR=/var/lib/rancher/k3s
  for _ in $(seq 1 60); do
    systemctl is-active --quiet "$K3S_SERVICE" && break
    sleep 1
  done
  detect_native_k3s \
    || die "k3s installation completed but $K3S_SERVICE did not become active"
  select_native_kubeconfig
}

bootstrap_install_kubectl() {
  if command -v kubectl >/dev/null 2>&1 \
    && kubectl version --client >/dev/null 2>&1; then
    note "Using kubectl at $(command -v kubectl)"
    return 0
  fi

  require_command curl
  require_command sha256sum
  require_command install

  local machine arch work url checksum
  machine="$(uname -m)"
  case "$machine" in
    x86_64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) die "kubectl auto-install does not support architecture $machine" ;;
  esac

  work="$(mktemp -d)"
  url="https://dl.k8s.io/release/$KUBECTL_INSTALL_VERSION/bin/linux/$arch/kubectl"

  note "Installing kubectl $KUBECTL_INSTALL_VERSION in $HOME/.local/bin"
  curl -fsSL "$url" -o "$work/kubectl"
  curl -fsSL "$url.sha256" -o "$work/kubectl.sha256"
  checksum="$(tr -d '[:space:]' < "$work/kubectl.sha256")"
  printf '%s  %s\n' "$checksum" "$work/kubectl" | sha256sum --check --status \
    || die "kubectl checksum verification failed"
  install -D -m 0755 "$work/kubectl" "$HOME/.local/bin/kubectl"
  rm -rf "$work"
  hash -r
  require_command kubectl
  kubectl version --client >/dev/null 2>&1 \
    || die "the installed kubectl binary does not run"
}

installed_weaver_kube_version() {
  local binary="${1:-}"
  [[ -n "$binary" && -x "$binary" ]] || return 1
  go version -m "$binary" 2>/dev/null \
    | awk '$1 == "mod" && $2 == "github.com/ServiceWeaver/weaver-kube" { print $3; exit }'
}

bootstrap_install_weaver_kube() {
  local binary installed
  binary="$(command -v weaver-kube 2>/dev/null || true)"
  installed="$(installed_weaver_kube_version "$binary" || true)"
  if [[ "$installed" == "$WEAVER_KUBE_INSTALL_VERSION" ]]; then
    note "Using weaver-kube $installed at $binary"
    return 0
  fi

  require_command go
  mkdir -p "$HOME/.local/bin"
  note "Installing weaver-kube $WEAVER_KUBE_INSTALL_VERSION in $HOME/.local/bin"
  GOBIN="$HOME/.local/bin" go install \
    "github.com/ServiceWeaver/weaver-kube/cmd/weaver-kube@$WEAVER_KUBE_INSTALL_VERSION"
  hash -r
  require_command weaver-kube
  installed="$(installed_weaver_kube_version "$(command -v weaver-kube)" || true)"
  [[ "$installed" == "$WEAVER_KUBE_INSTALL_VERSION" ]] \
    || die "installed weaver-kube version is ${installed:-unknown}, expected $WEAVER_KUBE_INSTALL_VERSION"
}

bootstrap_runtime_tools() {
  bootstrap_install_k3s
  bootstrap_install_kubectl
  bootstrap_install_weaver_kube

  ensure_native_k3s
  ensure_kubeconfig
  for _ in $(seq 1 60); do
    if kubectl --kubeconfig "$KUBECONFIG" get nodes >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  die "kubectl could not reach native k3s using $KUBECONFIG"
}
