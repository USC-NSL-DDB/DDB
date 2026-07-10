#!/bin/bash
# shellcheck shell=bash
#
# Shared helpers for the nu-socialnet-overhead experiment (distributed layout).
# Source from any script in this directory.
#
# Topology (indices are the last octet of the 10.10.x ssh network):
#   node0 = idx 1 = this header node : controller, client, init_graph, DDB, EMQX
#   node1 = idx 2 \
#   node2 = idx 3  |  Nu proclet servers, one ServiceEntry each
#   node3 = idx 4  |  caladan IPs 18.18.1.2 .. 18.18.1.5
#   node4 = idx 5 /   node4 is the "main" (-m) server that boots the app
#
# Caladan binds one Mellanox port via DPDK; Nu ssh'es over the *other* 10.10.x
# NIC. Seeding is done natively (build/init_graph over Thrift) so there is no
# nginx and no node needs its caladan NIC in kernel mode.

EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
[[ -z "$REPO_ROOT" ]] && REPO_ROOT="$(cd "$EXP_DIR/../../.." && pwd)"

NU_DIR="$REPO_ROOT/fwks/Nu"
SOCIALNET_DIR="$NU_DIR/app/socialNetwork/single_proclet"
CONNECTOR_DIR="$REPO_ROOT/connector"
DDB_BIN="$REPO_ROOT/ddb/target/release/ddb"

# ─── Roles ───────────────────────────────────────────────────────────────────
INFRA_IDX=1                 # node0 (this node): controller + client + init + DDB
SERVER_IDXS=(2 3 4 5)       # node1..node4
NUM_SERVERS=${#SERVER_IDXS[@]}
# The last server boots the app (creates the proclets); the rest just join.
MAIN_SERVER_IDX=${SERVER_IDXS[$((NUM_SERVERS - 1))]}

LPID=1
CTRL_CALADAN_IP=18.18.1.1   # hard-coded in ctrl_main.cpp; server default -c too
INIT_ENTRY_IP=18.18.1.2     # init_graph's kProxyIp -> the first server's entry

# server k (0-based) gets caladan IP 18.18.1.(k+2); the client's get_entry_ip(k)
# and init_graph both expect the entries to start at 18.18.1.2.
server_caladan_ip() { echo "18.18.1.$(( $1 + 2 ))"; }

DDB_SD_CONFIG=/tmp/ddb/service_discovery/config
LOG_DIR="$EXP_DIR/logs"
RESULTS_DIR="$EXP_DIR/results"

die() { echo "Error: $*" >&2; exit 1; }

# ─── NIC / addressing ────────────────────────────────────────────────────────

caladan_nic() {
  if [[ -n "${_CALADAN_NIC_CACHE:-}" ]]; then echo "$_CALADAN_NIC_CACHE"; return 0; fi
  local mac probe
  probe=$(mktemp)
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

# The 10.10.x network Nu uses for ssh (not the caladan NIC).
ssh_prefix() {
  local nic="$1"
  ip -o -4 addr show | awk -v nic="$nic" '$2 != nic && index($4, "10.10.") == 1 {
    split($4, a, "/"); split(a[1], b, "."); print b[1]"."b[2]"."b[3]"."; exit }'
}

node_ip() { echo "${SSH_PREFIX:?SSH_PREFIX not set}${1}"; }

remote() {  # $1 = node index, rest = command; runs to completion
  local idx="$1"; shift
  ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no "$(node_ip "$idx")" "$@"
}

# Start a long-lived remote process. `ssh host "cmd &"` hangs (sudo re-opens the
# ssh channel's stdio); `ssh -f` backgrounds ssh itself after auth, which works.
# The caller supplies redirects. Poll for the process; -f returns before it runs.
remote_bg() {  # $1 = node index, rest = command
  local idx="$1"; shift
  ssh -f -n -o BatchMode=yes -o StrictHostKeyChecking=no "$(node_ip "$idx")" "$*"
}

# Set CALADAN_NIC + SSH_PREFIX in the environment (both exported).
detect_network() {
  CALADAN_NIC="${CALADAN_NIC:-$(caladan_nic)}" || die "could not detect caladan NIC (is caladan built?)"
  export CALADAN_NIC
  SSH_PREFIX="${SSH_PREFIX:-$(ssh_prefix "$CALADAN_NIC")}"
  export SSH_PREFIX
  [[ -n "$SSH_PREFIX" ]] || die "could not find the non-caladan 10.10.x network"
}

require_built() {
  [[ -x "$NU_DIR/caladan/iokerneld" ]]           || die "caladan not built. Run ./build_all.sh"
  [[ -x "$NU_DIR/bin/ctrl_main" ]]               || die "Nu not built. Run ./build_all.sh"
  [[ -f "$SOCIALNET_DIR/build/src/main" ]]       || die "socialnet backend not built. Run ./build_all.sh"
  [[ -f "$SOCIALNET_DIR/build/bench/client" ]]   || die "socialnet client not built. Run ./build_all.sh"
  [[ -f "$SOCIALNET_DIR/build/init_graph/init_graph" ]] || die "init_graph not built. Run ./build_all.sh"
}

# A killed iokerneld leaves SysV shm + DPDK hugepage files behind; the next one
# then dies with "Shared memory region is already mapped". Clear both.
CALADAN_RESET_CMD='sudo pkill -9 gdb 2>/dev/null; sudo pkill -9 -x main; sudo pkill -9 -x client;
  sudo pkill -9 -x init_graph; sudo pkill -9 ctrl_main; sudo pkill -9 ctrl_proxy; sudo pkill -9 iokerneld;
  sleep 1;
  for id in $(ipcs -m | awk "\$6 ~ /^[0-9]+$/ {print \$2}"); do sudo ipcrm -m "$id" 2>/dev/null; done;
  sudo rm -f /dev/hugepages/rtemap_* /tmp/iokerneld.log /tmp/ctrl_main.log /tmp/backend.log 2>/dev/null'
