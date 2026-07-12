#!/bin/bash
# shellcheck shell=bash
#
# Shared config + helpers for the raft-lab DDB overhead experiment.
# Source from any script in this directory.
#
# Topology (fixed, hard-coded for the CloudLab profile this ships with):
#
#   node0  10.10.1.1  head: tput_remote (the load generator) + DDB + MQTT broker
#   node1  10.10.1.2  raft id 1  \
#   node2  10.10.1.3  raft id 2   |  the 3-node raft cluster, one process per node
#   node3  10.10.1.4  raft id 3  /
#
# Every command is driven from node0. Each raft node is alone on its machine, so
# they can all share the same two ports.
#
# The raft-lab sources are NOT part of this repo (private). See README.md.

EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
[[ -z "$REPO_ROOT" ]] && REPO_ROOT="$(cd "$EXP_DIR/../../.." && pwd)"

# ─── The raft-lab repo (external, private) ───────────────────────────────────
# Default location; override with RAFT_DIR=/path/to/raft-lab-cpp-solution.
RAFT_DIR="${RAFT_DIR:-/mnt/local/raft-lab-cpp-solution}"

# Third-party prefix for this experiment: the DDB-patched gRPC + the DDB
# connector headers. It lives on /mnt/local because the root filesystem on these
# images is 16G and gRPC alone does not fit.
DEPS_PREFIX="${DEPS_PREFIX:-/mnt/local/opt/raft-deps}"

RAFT_NODE_BIN="$RAFT_DIR/build/app/raft_node"
TPUT_BIN="$RAFT_DIR/build/app/tput_remote"
DDB_BIN="$REPO_ROOT/ddb/target/release/ddb"
CONNECTOR_DIR="$REPO_ROOT/connector"

# Toolchain. The image ships an experimental g++-13 (13.0.0, no <format>) at
# /usr/local/bin, which shadows the real one on PATH -- always use absolute paths.
CC_BIN="${CC_BIN:-/usr/bin/gcc-13}"
CXX_BIN="${CXX_BIN:-/usr/bin/g++-13}"

# ─── Topology ────────────────────────────────────────────────────────────────
HEAD_IP="${HEAD_IP:-10.10.1.1}"          # node0: client + DDB + broker
SERVER_IPS=(10.10.1.2 10.10.1.3 10.10.1.4)   # node1..node3: raft ids 1..3
NUM_SERVERS=${#SERVER_IPS[@]}

RAFT_PORT="${RAFT_PORT:-50051}"          # raft consensus port (same on each node)
TESTER_PORT="${TESTER_PORT:-55001}"      # tput_remote -> node control RPCs
CTRL_PORT="${CTRL_PORT:-55000}"          # nodes -> tput_remote ready callback

# raft id for the k-th (0-based) entry of SERVER_IPS
server_id() { echo "$(( $1 + 1 ))"; }

# --peers for the k-th server: "id+host:port,..." for every *other* node.
server_peers() {
  local self="$1" k id parts=()
  for k in $(seq 0 $((NUM_SERVERS - 1))); do
    [[ "$k" -eq "$self" ]] && continue
    id="$(server_id "$k")"
    parts+=("${id}+${SERVER_IPS[$k]}:${RAFT_PORT}")
  done
  local IFS=,; echo "${parts[*]}"
}

DDB_SD_CONFIG=/tmp/ddb/service_discovery/config
LOG_DIR="$EXP_DIR/logs"
RESULTS_DIR="$EXP_DIR/results"
NODES_JSON="$LOG_DIR/nodes.json"

die() { echo "Error: $*" >&2; exit 1; }

# ─── Remote helpers ──────────────────────────────────────────────────────────

remote() {  # $1 = host, rest = command; runs to completion
  local host="$1"; shift
  ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no "$host" "$@"
}

# Start a long-lived remote process. `ssh host "cmd &"` hangs; `ssh -f`
# backgrounds ssh itself after auth, which does not. The caller supplies the
# redirects -- the remote process logs to a file on its own node, so the ssh
# client's stdio goes to /dev/null (otherwise a later `kill` makes the remote
# shell's "Killed" job-control line leak back onto our terminal).
remote_bg() {  # $1 = host, rest = command
  local host="$1"; shift
  ssh -f -n -o BatchMode=yes -o StrictHostKeyChecking=no "$host" "$*" >/dev/null 2>&1
}

# ─── Preconditions ───────────────────────────────────────────────────────────

require_raft_dir() {
  [[ -d "$RAFT_DIR" ]] || die "raft-lab sources not found at: $RAFT_DIR

This experiment measures a *private* codebase that is not vendored into DDB.
Ask the DDB authors for read access to the repo, then:

    git clone https://github.com/USC-NSL/raft-lab-cpp-solution.git \\
        /mnt/local/raft-lab-cpp-solution

If it lives somewhere else, point this harness at it:

    RAFT_DIR=/path/to/raft-lab-cpp-solution ./$(basename "${BASH_SOURCE[1]:-run_benchmark.sh}")

See README.md ('Getting the raft-lab repo')."
  [[ -f "$RAFT_DIR/app/raft_node.cpp" ]] || die "$RAFT_DIR does not look like raft-lab-cpp-solution (no app/raft_node.cpp)"
}

require_built() {
  require_raft_dir
  [[ -x "$RAFT_NODE_BIN" ]] || die "raft_node not built ($RAFT_NODE_BIN). Run ./build_all.sh"
  [[ -x "$TPUT_BIN" ]]      || die "tput_remote not built ($TPUT_BIN). Run ./build_all.sh"
}

# Kill everything this experiment starts on a server node. `raft_node` ignores a
# plain SIGTERM while parked in sigwait under DDB, so use -9; gdb goes first so
# it detaches instead of leaving the inferior ptrace-stopped. The gdb-mode MI
# session reads from a fifo held open by a sleep (see run_benchmark.sh) -- clear
# both or the next run's mkfifo fails.
SERVER_RESET_CMD='sudo pkill -9 gdb 2>/dev/null; sudo pkill -9 -x raft_node 2>/dev/null;
  [ -f /tmp/gdb_mi_holder.pid ] && kill -9 "$(cat /tmp/gdb_mi_holder.pid)" 2>/dev/null;
  rm -f /tmp/gdb_mi_in /tmp/gdb_mi_holder.pid; true'
