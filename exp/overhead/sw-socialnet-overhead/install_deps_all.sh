#!/bin/bash
#
# Install everything one node needs: k3s, Go, Docker, and the python kubernetes
# client. Runs on every node in cluster.txt.
#
# deploy_all.sh pipes this file into `ssh <node> bash -s`, so it must stay
# SELF-CONTAINED — it cannot source common.sh.
#
# The upstream cloudlab_profile setup.sh chains every step with `&&` and runs
# under `set -e`, so a single hiccup silently skips everything after it. We run
# it best-effort, then verify and repair each dependency ourselves.

set -uo pipefail

GO_VERSION="1.22.4"
# Pinned so every node runs the same k3s AND so the installer skips its
# update.k3s.io channel lookup — that endpoint intermittently serves a TLS
# chain this image's openssl cannot verify, which made installs fail at random.
K3S_VERSION="${K3S_VERSION:-v1.36.2+k3s1}"
K3S_CONFIG="/etc/rancher/k3s/config.yaml"
SETUP_URL="https://raw.githubusercontent.com/hjzccc/cloudlab_profile/refs/heads/main/setup.sh"

log()  { echo "[$(hostname -s)] $*"; }
fail() { echo "[$(hostname -s)] ERROR: $*" >&2; exit 1; }

# Query the group database, not the current process credentials: a fresh
# `usermod -aG` is visible to `id -nG <user>` immediately, but not to `id -nG`.
in_docker_group() { case " $(id -nG "$(whoami)" 2>/dev/null) " in *" docker "*) return 0 ;; *) return 1 ;; esac; }

# ─── 1. cgroup v1 opt-out, BEFORE k3s is installed or started ────────────────
# Kubernetes >= 1.35 refuses to start a kubelet on a cgroup v1 host, and
# get.k3s.io always installs the latest k3s. Without this, k3s crash-loops, the
# upstream script's `k3s kubectl get node` check fails, `set -e` fires, and Go /
# python / weaver-kube never install.
if [[ "$(stat -fc %T /sys/fs/cgroup)" != "cgroup2fs" ]]; then
  if ! sudo grep -qs 'fail-cgroupv1=false' "$K3S_CONFIG"; then
    if sudo test -s "$K3S_CONFIG"; then
      fail "$K3S_CONFIG already has content. Add this to it by hand, then re-run:
    kubelet-arg:
      - \"fail-cgroupv1=false\""
    fi
    log "cgroup v1 host: writing $K3S_CONFIG"
    sudo mkdir -p "$(dirname "$K3S_CONFIG")"
    # The marker tells setup_experiment.sh this file is ours to rewrite (it adds
    # node-ip / flannel-iface on the master).
    printf '# managed by sw-socialnet-overhead\nkubelet-arg:\n  - "fail-cgroupv1=false"\n' \
      | sudo tee "$K3S_CONFIG" >/dev/null
  fi
fi

# ─── 2. Upstream provisioning (best effort) ──────────────────────────────────
# Installs k3s/Go/Docker and drops ssh_gateway.yaml + setup_debug_container.py
# into /local/tmp. Its exit status is not trustworthy, so we ignore it.
#
# setup.sh's FIRST step wgets into /local/tmp, which CloudLab provisions as
# geniuser-owned 0755 — unwritable for us. And since setup.sh chains every step
# with `&&`, that one EACCES silently skips the k3s/Go/Docker installs too. Own
# the directory first, and clear stale copies of its downloads: wget never
# overwrites (it saves to *.1), while setup.sh would chmod + run the OLD file.
log "claiming /local/tmp for $(whoami)"
sudo mkdir -p /local/tmp
sudo chown -R "$(whoami):" /local/tmp
rm -f /local/tmp/install_dependencis.sh* /local/tmp/ssh_gateway.yaml* \
      /local/tmp/serviceweaver_config.yaml* /local/tmp/setup_debug_container.py*
# Its output is noisy and its failures are expected — keep it out of the
# operator's face; the log stays on the node for debugging.
log "running cloudlab_profile setup.sh (best effort, log: /tmp/cloudlab_setup.log)"
wget -qO- "$SETUP_URL" | bash >/tmp/cloudlab_setup.log 2>&1 \
  || log "setup.sh exited non-zero — repairing below"

export PATH="/usr/local/go/bin:$HOME/go/bin:$PATH"

# ─── 3. k3s (setup.sh's install is flaky; repair it ourselves) ───────────────
# Pinning INSTALL_K3S_VERSION makes the installer download straight from the
# GitHub release, skipping the unreliable update.k3s.io channel resolution.
if ! command -v k3s >/dev/null 2>&1; then
  log "installing k3s $K3S_VERSION"
  ok=0
  for attempt in 1 2 3; do
    if curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION="$K3S_VERSION" sh - \
         >>/tmp/cloudlab_setup.log 2>&1; then ok=1; break; fi
    log "k3s install attempt $attempt failed; retrying"
    sleep 5
  done
  [[ "$ok" -eq 1 ]] || fail "k3s install failed 3 times (see /tmp/cloudlab_setup.log)"
fi

# ─── 4. Go ───────────────────────────────────────────────────────────────────
if ! command -v go >/dev/null 2>&1; then
  log "installing Go $GO_VERSION"
  case "$(uname -m)" in
    x86_64)  go_arch=amd64 ;;
    aarch64) go_arch=arm64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
  esac
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/go.tgz" "https://go.dev/dl/go${GO_VERSION}.linux-${go_arch}.tar.gz" \
    || fail "failed to download Go $GO_VERSION"
  sudo rm -rf /usr/local/go
  sudo tar -C /usr/local -xzf "$tmp/go.tgz" || fail "failed to unpack Go"
  rm -rf "$tmp"
fi

# Non-interactive shells don't source ~/.bashrc, so later scripts add
# /usr/local/go/bin themselves; persist it here for interactive logins.
for line in 'export PATH=$PATH:/usr/local/go/bin' 'export PATH=$PATH:$HOME/go/bin'; do
  grep -qxF "$line" ~/.bashrc 2>/dev/null || echo "$line" >> ~/.bashrc
done

# ─── 5. python kubernetes client (needed by setup_ddb.sh) ────────────────────
# pip < 23.0.1 has no --break-system-packages; newer pip on an externally
# managed install requires it. Try plain first, then fall back.
if ! python3 -c 'import kubernetes' 2>/dev/null; then
  log "installing python kubernetes client"
  pip3 install --user kubernetes 2>/dev/null \
    || pip3 install --user --break-system-packages kubernetes \
    || fail "could not install the python kubernetes client"
fi

# ─── 6. Docker group (takes effect in a new login shell) ─────────────────────
if ! in_docker_group; then
  log "adding $(whoami) to the docker group"
  sudo usermod -aG docker "$(whoami)" || fail "usermod failed"
fi

# ─── 7. Verify, so a broken node cannot report success ───────────────────────
command -v k3s    >/dev/null 2>&1 || fail "k3s not installed"
command -v docker >/dev/null 2>&1 || fail "docker not installed"
command -v go     >/dev/null 2>&1 || fail "Go not on PATH after install"
python3 -c 'import kubernetes' 2>/dev/null || fail "python kubernetes client not importable"
in_docker_group || fail "$(whoami) is not in the docker group"

log "OK  k3s=$(k3s --version 2>/dev/null | head -1 | awk '{print $3}')  go=$(go version | awk '{print $3}')  docker=$(docker --version | awk '{print $3}' | tr -d ,)"
