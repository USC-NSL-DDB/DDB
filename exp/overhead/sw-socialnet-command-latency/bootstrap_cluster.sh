#!/usr/bin/env bash
# Install and assemble the five-node native-k3s cluster from the controller.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

readonly K3S_CONFIG=/etc/rancher/k3s/config.yaml
readonly K3S_CONFIG_MARKER="# managed by sw-socialnet-command-latency"
ALLOW_ACTIVE_FIREWALL="${ALLOW_ACTIVE_FIREWALL:-0}"

firewall_guard() {
  if systemctl is-active --quiet firewalld 2>/dev/null \
    && [[ "$ALLOW_ACTIVE_FIREWALL" != "1" ]]; then
    die "firewalld is active on the controller. The k3s overlay requires the private cluster network to be allowed.
  Configure firewalld and set ALLOW_ACTIVE_FIREWALL=1, or disable it explicitly:
    sudo systemctl disable --now firewalld"
  fi
}

controller_iface() {
  ip -o -4 addr show \
    | awk -v ip="$CONTROLLER_IP" '$4 == ip || index($4, ip "/") == 1 { print $2; exit }'
}

desired_controller_config() {
  local iface="$1"
  echo "$K3S_CONFIG_MARKER"
  printf 'write-kubeconfig-mode: "0644"\n'
  printf 'node-name: "%s"\n' "$TARGET_NODE"
  printf 'node-ip: "%s"\n' "$CONTROLLER_IP"
  printf 'flannel-iface: "%s"\n' "$iface"
  printf 'disable:\n'
  printf '  - traefik\n  - servicelb\n  - metrics-server\n  - local-storage\n'
  if [[ "$(stat -fc %T /sys/fs/cgroup)" != "cgroup2fs" ]]; then
    printf 'kubelet-arg:\n  - "fail-cgroupv1=false"\n'
  fi
}

install_controller_k3s() {
  require_command curl
  require_command ip
  require_command sudo
  require_command systemctl
  sudo -n true
  firewall_guard

  if systemctl cat k3s-agent.service >/dev/null 2>&1; then
    die "this controller has a k3s-agent service. Remove that agent before creating the control plane."
  fi

  local iface desired current work installer installed_version
  iface="$(controller_iface)"
  [[ -n "$iface" ]] || die "could not find the interface carrying CONTROLLER_IP $CONTROLLER_IP"
  desired="$(desired_controller_config "$iface")"
  current="$(sudo cat "$K3S_CONFIG" 2>/dev/null || true)"
  if [[ -n "$current" && "$current" != "$desired" \
    && "$current" != *"$K3S_CONFIG_MARKER"* ]]; then
    die "$K3S_CONFIG already exists and is not recipe-managed.
  Merge the controller node-ip and flannel-iface settings manually, or remove it after confirming it is safe."
  fi

  sudo mkdir -p "$(dirname "$K3S_CONFIG")"
  printf '%s\n' "$desired" | sudo tee "$K3S_CONFIG" >/dev/null

  installed_version="$(k3s --version 2>/dev/null | awk 'NR == 1 { print $3 }' || true)"
  if [[ "$installed_version" != "$K3S_INSTALL_VERSION" ]] \
    || ! systemctl cat k3s.service >/dev/null 2>&1; then
    work="$(mktemp -d)"
    installer="$work/install-k3s.sh"
    note "Installing native k3s server $K3S_INSTALL_VERSION on the controller"
    curl -fsSL https://get.k3s.io -o "$installer"
    sudo env INSTALL_K3S_VERSION="$K3S_INSTALL_VERSION" \
      INSTALL_K3S_EXEC=server sh "$installer"
    rm -rf "$work"
  else
    note "Using native k3s $installed_version on the controller"
    sudo systemctl enable k3s.service >/dev/null
    sudo systemctl restart k3s.service
  fi

  for _ in $(seq 1 90); do
    if systemctl is-active --quiet k3s.service \
      && sudo k3s kubectl get node "$TARGET_NODE" >/dev/null 2>&1; then
      K3S_SERVICE=k3s.service
      K3S_BIN="$(command -v k3s)"
      installed_version="$(k3s --version | awk 'NR == 1 { print $3 }')"
      [[ "$installed_version" == "$K3S_INSTALL_VERSION" ]] \
        || die "controller k3s version is $installed_version, expected $K3S_INSTALL_VERSION"
      return 0
    fi
    sleep 2
  done
  die "the k3s server did not become ready; inspect: sudo journalctl -u k3s -n 80 --no-pager"
}

select_kubeconfig() {
  if [[ "$KUBECONFIG_EXPLICIT" -eq 1 ]]; then
    [[ -r "$KUBECONFIG" ]] || die "configured kubeconfig is not readable: $KUBECONFIG"
    return 0
  fi
  KUBECONFIG=/etc/rancher/k3s/k3s.yaml
  if [[ ! -r "$KUBECONFIG" ]]; then
    local target="$HOME/.kube/sw-socialnet-command-latency-k3s.yaml"
    mkdir -p "$(dirname "$target")"
    sudo install -m 0600 -o "$(id -u)" -g "$(id -g)" "$KUBECONFIG" "$target"
    KUBECONFIG="$target"
  fi
  export KUBECONFIG
}

install_kubectl() {
  if command -v kubectl >/dev/null 2>&1 \
    && kubectl version --client >/dev/null 2>&1; then
    note "Using kubectl at $(command -v kubectl)"
    return 0
  fi

  require_command curl
  require_command install
  require_command sha256sum
  local arch work url checksum
  case "$(uname -m)" in
    x86_64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) die "kubectl auto-install does not support architecture $(uname -m)" ;;
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
}

installed_weaver_kube_version() {
  local binary="${1:-}"
  [[ -n "$binary" && -x "$binary" ]] || return 1
  go version -m "$binary" 2>/dev/null \
    | awk '$1 == "mod" && $2 == "github.com/ServiceWeaver/weaver-kube" { print $3; exit }'
}

install_weaver_kube() {
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
  installed="$(installed_weaver_kube_version "$(command -v weaver-kube 2>/dev/null || true)" || true)"
  [[ "$installed" == "$WEAVER_KUBE_INSTALL_VERSION" ]] \
    || die "installed weaver-kube version is ${installed:-unknown}, expected $WEAVER_KUBE_INSTALL_VERSION"
}

preflight_workers() {
  local target record
  local -a node_names=("$TARGET_NODE")
  for target in "${WORKER_TARGETS[@]}"; do
    note "Checking SSH, sudo, firewall, and k3s role on $target"
    if ! record="$(worker_ssh "$target" bash -s -- \
      "$ALLOW_ACTIVE_FIREWALL" "$CONTROLLER_IP" <<'WORKER_CHECK'
set -euo pipefail
allow_firewall="$1"
controller_ip="$2"
for command in curl systemctl ip; do
  command -v "$command" >/dev/null \
    || { echo "Error: $command is missing on $(hostname)" >&2; exit 1; }
done
sudo -n true \
  || { echo "Error: passwordless sudo is required on worker $(hostname)" >&2; exit 1; }
if systemctl is-active --quiet firewalld 2>/dev/null && [[ "$allow_firewall" != "1" ]]; then
  echo "Error: firewalld is active on $(hostname). Configure it or run: sudo systemctl disable --now firewalld" >&2
  exit 1
fi
if systemctl cat k3s.service >/dev/null 2>&1; then
  echo "Error: $(hostname) has a k3s server service but must be an agent. Confirm this worker is disposable, run sudo /usr/local/bin/k3s-uninstall.sh, then rerun setup." >&2
  exit 1
fi
route="$(ip -o route get "$controller_ip" | awk 'NR == 1')"
grep -q ' dev ' <<<"$route" && grep -q ' src ' <<<"$route" \
  || { echo "Error: no private route to controller $controller_ip on $(hostname)" >&2; exit 1; }
hostname
WORKER_CHECK
    )"
    then
      die "worker prerequisite check failed on $target"
    fi
    [[ -n "$record" ]] || die "worker $target returned an empty hostname"
    node_names+=("$record")
  done
  [[ "$(printf '%s\n' "${node_names[@]}" | sort -u | wc -l)" -eq "$EXPECTED_CLUSTER_NODES" ]] \
    || die "controller and worker hostnames must be unique: ${node_names[*]}"
}

join_worker() {
  local target="$1" token="$2"
  worker_ssh "$target" bash -s -- \
    "$CONTROLLER_IP" "$token" "$K3S_INSTALL_VERSION" "$ALLOW_ACTIVE_FIREWALL" <<'WORKER'
set -euo pipefail
controller_ip="$1"
token="$2"
k3s_version="$3"
allow_firewall="$4"

if systemctl is-active --quiet firewalld 2>/dev/null && [[ "$allow_firewall" != "1" ]]; then
  echo "Error: firewalld is active on $(hostname). Configure the private cluster network or run: sudo systemctl disable --now firewalld" >&2
  exit 1
fi
if systemctl cat k3s.service >/dev/null 2>&1; then
  echo "Error: $(hostname) has a k3s server service but must be an agent. Confirm this worker is disposable, run sudo /usr/local/bin/k3s-uninstall.sh, then rerun setup." >&2
  exit 1
fi

route="$(ip -o route get "$controller_ip" | awk 'NR == 1')"
iface="$(awk '{for (i=1; i<=NF; i++) if ($i == "dev") {print $(i+1); exit}}' <<<"$route")"
worker_ip="$(awk '{for (i=1; i<=NF; i++) if ($i == "src") {print $(i+1); exit}}' <<<"$route")"
node_name="$(hostname)"
[[ -n "$iface" && -n "$worker_ip" && -n "$node_name" ]] \
  || { echo "Error: could not resolve the private route to $controller_ip on $(hostname)" >&2; exit 1; }

agent_args="agent --node-name=$node_name --node-ip=$worker_ip --flannel-iface=$iface"
if [[ "$(stat -fc %T /sys/fs/cgroup)" != "cgroup2fs" ]]; then
  agent_args+=" --kubelet-arg=fail-cgroupv1=false"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl -fsSL https://get.k3s.io -o "$work/install-k3s.sh"
sudo env \
  INSTALL_K3S_VERSION="$k3s_version" \
  K3S_URL="https://$controller_ip:6443" \
  K3S_TOKEN="$token" \
  INSTALL_K3S_EXEC="$agent_args" \
  sh "$work/install-k3s.sh" >&2

for _ in $(seq 1 60); do
  systemctl is-active --quiet k3s-agent.service && break
  sleep 1
done
systemctl is-active --quiet k3s-agent.service \
  || { echo "Error: k3s-agent did not become active on $(hostname)" >&2; exit 1; }
installed_version="$(k3s --version | awk 'NR == 1 { print $3 }')"
[[ "$installed_version" == "$k3s_version" ]] \
  || { echo "Error: k3s version is $installed_version on $(hostname), expected $k3s_version" >&2; exit 1; }
printf '%s\t%s\t%s\n' "$node_name" "$worker_ip" "$iface"
WORKER
}

wait_for_cluster() {
  local inventory="$1"
  local -a expected=("$TARGET_NODE")
  local node
  while IFS=$'\t' read -r node _; do
    [[ -z "$node" ]] || expected+=("$node")
  done < "$inventory"
  [[ "$(printf '%s\n' "${expected[@]}" | sort -u | wc -l)" -eq "$EXPECTED_CLUSTER_NODES" ]] \
    || die "controller and worker node names are not unique: ${expected[*]}"

  note "Waiting for all $EXPECTED_CLUSTER_NODES native-k3s nodes"
  for _ in $(seq 1 90); do
    local ready=0
    for node in "${expected[@]}"; do
      if [[ "$(kubectl --kubeconfig "$KUBECONFIG" get node "$node" \
        -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null)" == "True" ]]; then
        ready=$((ready + 1))
      fi
    done
    [[ "$ready" -eq "$EXPECTED_CLUSTER_NODES" ]] && break
    sleep 2
  done

  local actual
  actual="$(kubectl --kubeconfig "$KUBECONFIG" get nodes --no-headers | wc -l)"
  [[ "$actual" -eq "$EXPECTED_CLUSTER_NODES" ]] \
    || die "expected exactly $EXPECTED_CLUSTER_NODES Kubernetes nodes, found $actual; remove stale or unrelated node objects"
  for node in "${expected[@]}"; do
    [[ "$(kubectl --kubeconfig "$KUBECONFIG" get node "$node" \
      -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}')" == "True" ]] \
      || die "Kubernetes node $node did not become Ready"
  done

  kubectl --kubeconfig "$KUBECONFIG" label node "$TARGET_NODE" \
    ddb-artifact-role=controller --overwrite >/dev/null
  kubectl --kubeconfig "$KUBECONFIG" taint node "$TARGET_NODE" \
    node-role.kubernetes.io/control-plane=:NoSchedule --overwrite >/dev/null
  for node in "${expected[@]:1}"; do
    kubectl --kubeconfig "$KUBECONFIG" label node "$node" \
      ddb-artifact-role=worker --overwrite >/dev/null
    kubectl --kubeconfig "$KUBECONFIG" uncordon "$node" >/dev/null 2>&1 || true
  done
}

main() {
  require_command awk
  require_command curl
  require_command ip
  require_command ssh
  require_command systemctl
  validate_cluster_inputs
  preflight_workers
  install_controller_k3s
  select_kubeconfig
  install_kubectl
  install_weaver_kube

  for _ in $(seq 1 60); do
    kubectl --kubeconfig "$KUBECONFIG" get nodes >/dev/null 2>&1 && break
    sleep 1
  done
  kubectl --kubeconfig "$KUBECONFIG" get nodes >/dev/null 2>&1 \
    || die "kubectl could not reach the controller using $KUBECONFIG"

  local token inventory target record
  token="$(sudo cat "$K3S_DATA_DIR/server/node-token")"
  [[ -n "$token" ]] || die "could not read the k3s node token"
  mkdir -p "$ARTIFACT_DIR/runtime"
  inventory="$ARTIFACT_DIR/runtime/cluster-nodes.tsv"
  : > "$inventory"
  for target in "${WORKER_TARGETS[@]}"; do
    note "Installing or reconciling the k3s agent on $target"
    record="$(join_worker "$target" "$token")"
    [[ "$record" == *$'\t'* ]] || die "worker $target returned an invalid node record"
    printf '%s\t%s\n' "$target" "$record" >> "$inventory"
  done

  # Strip the SSH-target column while checking Kubernetes node names.
  cut -f2- "$inventory" > "$inventory.nodes"
  wait_for_cluster "$inventory.nodes"
  mv "$inventory.nodes" "$inventory"
  note "Applying non-persistent controller TCP settings required by the graph seeder"
  sudo sysctl -w net.ipv4.tcp_tw_reuse=1 >/dev/null
  sudo sysctl -w 'net.ipv4.ip_local_port_range=1024 65535' >/dev/null
  sudo sysctl -w net.ipv4.tcp_fin_timeout=15 >/dev/null
  note "Five-node native-k3s cluster is Ready"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
