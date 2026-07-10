#!/bin/bash
# shellcheck shell=bash
#
# Shared helpers for the nu-socialnet-overhead experiment.
# Source from any script in this directory.

EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Ask git for the repo root rather than counting `..` levels.
REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
if [[ -z "$REPO_ROOT" ]]; then
  REPO_ROOT="$(cd "$EXP_DIR/../../.." && pwd)"
fi

NU_DIR="$REPO_ROOT/fwks/Nu"
SOCIALNET_DIR="$NU_DIR/app/socialNetwork/single_proclet"
CONNECTOR_DIR="$REPO_ROOT/connector"
DDB_BIN="$REPO_ROOT/ddb/target/release/ddb"

# Node roles, by Nu's ssh index (shared.sh: ssh_ip N -> <ssh_prefix>.N).
# The backend MUST stay at index 1: nginx's config hard-codes its caladan IP.
BACKEND_IDX=1
CTRL_IDX=2
NGINX_IDX=3
CLIENT_IDX=4

# nginx needs an address on the caladan network to reach the Nu backend.
NGINX_CALADAN_IP_AND_MASK=18.18.1.254/24

# Written by DDB when it starts its broker; read by every Nu process that is
# launched with --ddb. Must exist on every server node.
DDB_SD_CONFIG=/tmp/ddb/service_discovery/config

LOG_DIR="$EXP_DIR/logs"
RESULTS_DIR="$EXP_DIR/results"

die() { echo "Error: $*" >&2; exit 1; }

# Caladan binds one Mellanox port via DPDK; Nu ssh'es over the *other* 10.10.x
# NIC. Both are derived the same way Nu's exp/shared.sh does it.
# Probing costs ~15s (it briefly starts iokerneld), so cache it.
caladan_nic() {
  if [[ -n "${_CALADAN_NIC_CACHE:-}" ]]; then echo "$_CALADAN_NIC_CACHE"; return 0; fi

  local mac probe
  probe=$(mktemp)
  # iokerneld never exits on its own; run it detached and kill it once it has
  # printed the MAC of the port DPDK bound. Its exit status is meaningless.
  sudo timeout 20 "$NU_DIR/caladan/iokerneld" >"$probe" 2>&1 &
  for _ in $(seq 1 20); do grep -q "MAC" "$probe" 2>/dev/null && break; sleep 1; done
  sudo pkill -9 iokerneld 2>/dev/null || true
  wait 2>/dev/null || true

  mac=$(grep "MAC" "$probe" 2>/dev/null | sed 's/.*MAC: \(.*\)/\1/' | tr ' ' ':')
  rm -f "$probe"
  [[ -n "$mac" ]] || return 1
  _CALADAN_NIC_CACHE=$(ip -o link | grep -i "$mac" | awk -F': ' '{print $2; exit}')
  [[ -n "$_CALADAN_NIC_CACHE" ]] || return 1
  echo "$_CALADAN_NIC_CACHE"
}

# The 10.10.x network Nu uses for ssh (i.e. not the caladan NIC).
ssh_prefix() {
  local nic="$1"
  ip -o -4 addr show | awk -v nic="$nic" '$2 != nic && index($4, "10.10.") == 1 {
    split($4, a, "/"); split(a[1], b, "."); print b[1]"."b[2]"."b[3]"."; exit }'
}

ssh_ip() {  # $1 = node index
  local nic pfx
  nic="${CALADAN_NIC:-$(caladan_nic)}"
  pfx="$(ssh_prefix "$nic")"
  echo "${pfx}${1}"
}

# Cheap variant that avoids probing the NIC every call.
node_ip() { echo "${SSH_PREFIX:?SSH_PREFIX not set}${1}"; }

remote() {  # $1 = node index, rest = command. Runs to completion.
  local idx="$1"; shift
  ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no "$(node_ip "$idx")" "$@"
}

# Start a long-lived remote process (iokerneld, ctrl_main, the Nu backend).
#
# `ssh host "cmd &"` does NOT work here: sshd keeps the channel open as long as
# any process holds its stdout/stderr, and `sudo` re-opens them, so the ssh
# never returns. `setsid` doesn't help either. `ssh -f` backgrounds ssh itself
# after authentication, which does. Poll for the process rather than trusting
# the exit status -- ssh -f returns before the command has run.
# The caller supplies its own redirects (e.g. ">/tmp/x.log 2>&1").
remote_bg() {  # $1 = node index, rest = command
  local idx="$1"; shift
  ssh -f -n -o BatchMode=yes -o StrictHostKeyChecking=no "$(node_ip "$idx")" "$*"
}

require_built() {
  [[ -x "$NU_DIR/caladan/iokerneld" ]] || die "caladan not built. Run ./build_all.sh"
  [[ -x "$NU_DIR/bin/ctrl_main" ]]     || die "Nu not built. Run ./build_all.sh"
  [[ -f "$SOCIALNET_DIR/build/src/main" ]] || die "socialnet backend not built. Run ./build_all.sh"
  [[ -f "$SOCIALNET_DIR/build/bench/client" ]] || die "socialnet client not built. Run ./build_all.sh"
}
