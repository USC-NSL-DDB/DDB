#!/bin/bash
# shellcheck shell=bash
#
# Shared config + helpers for the PET perceived-time-gap experiment.
# Source from any script in this directory.
#
# What the experiment measures
# ----------------------------
# When DDB stops a process, real time keeps running but the process does not. On
# resume the application would normally see the whole pause as one enormous jump
# in the wall clock, which is enough to trip timeouts, blow up rate limiters and
# generally make a debugged distributed system fall apart.
#
# DDB hides the pause: libfaketime (LD_PRELOAD'd into the app) subtracts an offset
# from every clock call, and DDB's gdb extension grows that offset by the exact
# duration of each pause before it resumes the process. The offset is *dynamic* --
# it is rewritten, in place, in the debuggee's environ block, on every resume.
#
# This experiment quantifies the residual: after the correction, how much time
# does the application still perceive across a debugger pause? Ideally zero.
#
# Everything runs on one machine. DDB, the MQTT broker and the app are all local.

EXP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO_ROOT="$(git -C "$EXP_DIR" rev-parse --show-toplevel 2>/dev/null)"
[[ -z "$REPO_ROOT" ]] && REPO_ROOT="$(cd "$EXP_DIR/../.." && pwd)"

DDB_BIN="$REPO_ROOT/ddb/target/release/ddb"
LIBFAKETIME_DIR="$REPO_ROOT/libfaketime"
LIBFAKETIME_SO="$LIBFAKETIME_DIR/src/libfaketime.so.1"
APP_BIN="$EXP_DIR/build/faketime_pause"

LOG_DIR="$EXP_DIR/logs"
RESULTS_DIR="$EXP_DIR/results"

# DDB writes the broker address here; the app's connector reads it to find the
# broker and report itself. Must match connector/include/ddb/service_reporter.hpp.
DDB_SD_CONFIG=/tmp/ddb/service_discovery/config

# ─── Knobs ───────────────────────────────────────────────────────────────────
# The address the app reports itself on, and that DDB ssh'es back to in order to
# attach gdb. Loopback keeps this a genuinely single-node experiment.
APP_IP="${APP_IP:-127.0.0.1}"
BROKER_IP="${BROKER_IP:-127.0.0.1}"

# How long the app runs, in *real* seconds (it times itself with rdtsc, so the
# faketime offset cannot stretch or shrink this).
DURATION_SEC="${DURATION_SEC:-30}"

# Inner loop length. Sets the sampling interval -- the app records one
# (perceived, real) pair per iteration. The default lands around 50-100us per
# sample, i.e. two to three orders of magnitude finer than the pauses we inject,
# so a pause is unambiguous in the trace.
WORK_SIZE="${WORK_SIZE:-50000}"

# The pause train: how many times to stop the app, how long to hold it stopped,
# and how long to let it run in between. PAUSE_MS is deliberately much larger
# than the ~100us sampling interval so that "did the app perceive the pause?" is
# not a question about measurement noise.
NUM_PAUSES="${NUM_PAUSES:-20}"
PAUSE_MS="${PAUSE_MS:-100}"
GAP_MS="${GAP_MS:-500}"

# ─── libfaketime ─────────────────────────────────────────────────────────────
# The FAKETIME value is a *fractional-second offset* (libfaketime parses a leading
# '-' with atof, so "-0.0123" means "pretend it is 12.3ms earlier than it is").
#
# The zero padding is load-bearing. DDB's gdb extension updates the offset by
# writing the new string directly over the old one inside the debuggee's environ
# block (see modify_env_variable() in ddb/core/assets/gdb_ext/runtime-gdb.py) --
# it cannot grow the allocation. So the initial value has to be at least as long
# as the longest value we will ever write into it. 17 digits is room to spare: the
# offset only ever accumulates the total time we spend paused, so even a
# multi-hour session stays far inside it.
#
# FAKETIME_NO_CACHE=1 is the other half of "dynamic". By default libfaketime
# re-reads FAKETIME only every 10s; without this the app would keep using a stale
# offset for the first several pauses, and the experiment would show a gap that
# is an artifact of the cache rather than of the mechanism.
FAKETIME_INITIAL="${FAKETIME_INITIAL:--00000000000000000}"

die() { echo "Error: $*" >&2; exit 1; }

# ─── Watching the debuggee ───────────────────────────────────────────────────
# The single-letter process state from /proc/<pid>/stat. 't' means ptrace-stopped,
# i.e. gdb is actually holding the process; 'S'/'R' mean it is running.
#
# This matters more than it looks. An MI command written to DDB's fifo is not
# applied the instant we write it -- it is queued, shipped to gdb over SSH and
# applied asynchronously, which takes long enough to matter at the timescale of
# these pauses. Sleeping between two *writes* therefore does not hold the process
# stopped for that long: an early attempt at this sent -exec-interrupt, slept
# 100ms, sent -record-time-and-continue, and the process turned out to have been
# stopped for only ~75us, because both commands were still sitting in the queue
# together and gdb applied them back to back. Waiting for the state to actually
# become 't' is what makes the pause a real, controlled duration.
#
# The comm field in /proc/<pid>/stat is parenthesised and may contain spaces, so
# the state is taken as the first field after the closing paren, not as $3.
proc_state() {  # $1 = pid
  local line
  line="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
  line="${line##*) }"
  echo "${line%% *}"
}

# Block until pid $1 reaches state $2 ('t' to wait for stopped, '!t' for resumed).
# Returns 1 on timeout ($3 seconds, default 10) or if the process is gone.
wait_for_state() {  # $1 = pid, $2 = state or !state, $3 = timeout_s
  local pid="$1" want="$2" timeout="${3:-10}" negate=0 st
  [[ "$want" == !* ]] && { negate=1; want="${want#!}"; }
  local deadline=$(( $(date +%s) + timeout ))
  while :; do
    st="$(proc_state "$pid")" || return 1
    if [[ "$negate" -eq 0 && "$st" == "$want" ]]; then return 0; fi
    if [[ "$negate" -eq 1 && "$st" != "$want" ]]; then return 0; fi
    [[ "$(date +%s)" -ge "$deadline" ]] && return 1
    sleep 0.002
  done
}

# ─── docker ──────────────────────────────────────────────────────────────────
# DDB shells out to `docker` (unqualified, no sudo) to run the managed EMQX
# broker, so whatever launches ddb needs the docker group in its *current*
# credentials. Being added to the group does not apply to an already-running
# login shell, which is the usual reason this fails. `sg docker -c` re-execs with
# the group applied and fixes it without asking the user to re-login.
#
# Prints a prefix the caller puts in front of a command, e.g.
#   $(docker_launch_prefix) "$DDB_BIN ..."
docker_launch_prefix() {
  if docker info >/dev/null 2>&1; then
    echo ""                       # already usable
  elif sg docker -c 'docker info' >/dev/null 2>&1; then
    echo "sg-docker"              # usable via sg
  else
    die "cannot talk to docker as $(whoami); DDB needs it to run the EMQX broker.
  Add yourself to the docker group:  sudo usermod -aG docker $(whoami)
  then start a new login shell (or run this script under: newgrp docker)"
  fi
}

# Run a command line with the docker group applied if that is what it takes.
run_with_docker() {  # $* = the full command line, as a single string
  if [[ "$(docker_launch_prefix)" == "sg-docker" ]]; then
    sg docker -c "$*"
  else
    bash -c "$*"
  fi
}

# ─── Preconditions ───────────────────────────────────────────────────────────

require_built() {
  [[ -x "$DDB_BIN" ]] || die "ddb not built: $DDB_BIN
  cargo build --release --manifest-path $REPO_ROOT/ddb/Cargo.toml
  (or just run ./build_all.sh)"
  [[ -f "$LIBFAKETIME_SO" ]] || die "libfaketime not built: $LIBFAKETIME_SO
  make -C $LIBFAKETIME_DIR/src
  (or just run ./build_all.sh)"
  [[ -x "$APP_BIN" ]] || die "app not built: $APP_BIN
  make -C $EXP_DIR
  (or just run ./build_all.sh)"
}

# DDB attaches gdb over SSH even when the target is this same machine, so this has
# to work non-interactively or the app will sit in sigwait forever.
require_ssh() {
  ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout=5 \
    "$APP_IP" true 2>/dev/null \
    || die "passwordless ssh to $APP_IP does not work.

DDB attaches gdb over SSH, even for a process on this same machine. Set up a key:

    ssh-keygen -t ed25519 -N '' -f ~/.ssh/id_ed25519      # if you have no key
    ssh-copy-id $APP_IP

then check:  ssh $APP_IP true"
}

# ptrace_scope=1 restricts a non-root gdb to attaching to its own descendants.
# The app is not a descendant of gdb, so gdb must be root -- which is what
# `Conf.sudo: true` in the config gives us. Fail early and loudly if sudo needs a
# password, rather than hanging on an invisible prompt inside an ssh session.
require_sudo() {
  sudo -n true 2>/dev/null \
    || die "passwordless sudo is required (gdb must attach as root: ptrace_scope=$(cat /proc/sys/kernel/yama/ptrace_scope 2>/dev/null || echo '?'))"
}

# Everything this experiment starts, on this machine. gdb goes first so it
# detaches cleanly instead of leaving the app ptrace-stopped forever.
#
# Every kill here is `|| true`: pkill exits 1 when nothing matched, which is the
# normal case on a clean start, and the callers run under `set -e`.
reset_local() {
  sudo pkill -9 gdb            2>/dev/null || true
  sudo pkill -9 -x faketime_pause 2>/dev/null || true
  pkill -9 -x ddb              2>/dev/null || true
  if [[ -f "$LOG_DIR/ddb_holder.pid" ]]; then
    kill -9 "$(cat "$LOG_DIR/ddb_holder.pid")" 2>/dev/null || true
    rm -f "$LOG_DIR/ddb_holder.pid"
  fi
  rm -f "$LOG_DIR/ddb_in" "$LOG_DIR/ddb.pid" "$LOG_DIR/app.pid"
  # Give the kernel a moment to reap them, so the next run's port/pid checks and
  # the EMQX container name do not collide with a corpse.
  sleep 0.3
  return 0
}
