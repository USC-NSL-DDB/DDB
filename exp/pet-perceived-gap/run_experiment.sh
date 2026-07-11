#!/bin/bash
#
# Measure the time gap an application perceives across a DDB pause.
#
#   ./run_experiment.sh                    # faketime, dynamic offset (the mechanism)
#   ./run_experiment.sh --mode baseline    # no faketime (the control)
#   ./run_experiment.sh --mode both        # both, back to back, then compare
#
# Sequence, for one mode:
#   1. start DDB (broker + MI REPL on a fifo)
#   2. start faketime_pause; it reports itself over MQTT and parks in sigwait
#   3. DDB discovers it, ssh'es back, attaches gdb, sends SIG40; the connector
#      re-traps with SIGTRAP so the process is sitting stopped under gdb
#   4. resume it, then interrupt/resume it NUM_PAUSES times, holding it stopped
#      for PAUSE_MS each time. The resume is -record-time-and-continue, which is
#      the command that makes DDB grow the faketime offset by the pause duration
#      before letting the process go.
#   5. the app stops itself after DURATION_SEC of *real* time and writes its
#      samples; analyze.py turns them into the perceived gap per pause.
#
# In baseline mode the app runs without libfaketime. DDB's gdb extension notices
# FAKETIME is absent and skips the correction entirely (see check_faketime_present
# in runtime-gdb.py), so the same pause train produces the uncorrected numbers.
#
# Knobs (all overridable from the environment, see common.sh):
#   DURATION_SEC NUM_PAUSES PAUSE_MS GAP_MS WORK_SIZE APP_IP BROKER_IP

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

MODE=faketime
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE="$2"; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done
case "$MODE" in
  faketime|baseline|both) ;;
  *) die "--mode must be one of: faketime, baseline, both" ;;
esac

require_built
require_ssh
require_sudo

mkdir -p "$LOG_DIR" "$RESULTS_DIR"

FIFO="$LOG_DIR/ddb_in"

# Send one MI command to DDB's REPL.
mi() { echo "$1" > "$FIFO"; }

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  echo ""
  echo "Cleaning up..."
  reset_local
  exit $rc
}

# ─────────────────────────────────────────────────────────────────────────────
run_one() {  # $1 = faketime | baseline
  local mode="$1"
  local stamp; stamp="$(date +%Y%m%d_%H%M%S)"
  local samples="$RESULTS_DIR/${mode}_${stamp}.csv"
  local app_log="$LOG_DIR/app_${mode}.log"

  echo ""
  echo "############################################################"
  echo "# mode=$mode  duration=${DURATION_SEC}s  pauses=${NUM_PAUSES}x${PAUSE_MS}ms  gap=${GAP_MS}ms"
  echo "############################################################"

  reset_local
  "$EXP_DIR/start_ddb.sh"

  # ── start the app ────────────────────────────────────────────────────────
  # In faketime mode the app is launched under libfaketime with a zero offset.
  # DDB rewrites that offset in place, in the app's environ block, on every
  # resume -- which is why the initial value is zero-padded (see common.sh).
  echo "Starting faketime_pause (mode=$mode)..."
  local -a env_common=(
    "PET_OUT=$samples"
    "PET_DURATION_SEC=$DURATION_SEC"
    "PET_WORK_SIZE=$WORK_SIZE"
    "PET_DDB_IP=$APP_IP"
  )
  if [[ "$mode" == "faketime" ]]; then
    env "${env_common[@]}" \
        LD_PRELOAD="$LIBFAKETIME_SO" \
        FAKETIME="$FAKETIME_INITIAL" \
        FAKETIME_NO_CACHE=1 \
        "$APP_BIN" > "$app_log" 2>&1 &
  else
    env "${env_common[@]}" "$APP_BIN" > "$app_log" 2>&1 &
  fi
  local app_pid=$!
  echo "$app_pid" > "$LOG_DIR/app.pid"
  echo "  pid $app_pid, samples -> $samples"

  # ── wait for DDB to attach ───────────────────────────────────────────────
  # The app parks in sigwait (state S) until DDB attaches. Once gdb has it, the
  # process goes to 't' (tracing stop). That transition is the attach: it needs
  # no log scraping and cannot race.
  echo "Waiting for DDB to discover and attach (ssh + gdb)..."
  wait_for_state "$app_pid" t 90 || die "DDB never attached to pid $app_pid within 90s.
  ddb log:  $LOG_DIR/ddb.log
  app log:  $app_log
$(tail -20 "$app_log" 2>/dev/null)"
  echo "  attached (pid $app_pid is ptrace-stopped)"

  # ── resume it ────────────────────────────────────────────────────────────
  # DDB's attach handshake is: attach -> SIG40 -> the connector's sigwait returns
  # -> it re-raises SIGTRAP so it stops again for inspection. Depending on where
  # we caught it we may be at the attach stop or at that SIGTRAP, so continue
  # until the app tells us it got past connector.init() and is actually running.
  echo "Resuming the app..."
  local running=0
  for _ in $(seq 1 30); do
    if grep -q '\[pet\] running' "$app_log" 2>/dev/null; then running=1; break; fi
    mi "-exec-continue"
    sleep 1
  done
  [[ "$running" -eq 1 ]] || die "app never got past the DDB attach handshake.
  ddb log:  $LOG_DIR/ddb.log
  app log:  $app_log
$(tail -20 "$app_log")"
  echo "  running"

  # Let it build up a stretch of undisturbed samples first. analyze.py uses the
  # quiet stretches to work out what a normal, pause-free sampling interval costs
  # on this machine, which is what the perceived gap is measured against.
  sleep 2

  # ── the pause train ──────────────────────────────────────────────────────
  # This is the experiment. -exec-interrupt stops the process; once it is really
  # stopped we hold it for PAUSE_MS, and then -record-time-and-continue tells
  # DDB's gdb extension to (a) work out how long the process was stopped, (b) add
  # that to the accumulated offset, (c) write the new offset over FAKETIME in the
  # debuggee's environ block, and only then (d) resume it.
  #
  # Plain -exec-continue would skip (a)-(c) and the app would see the whole pause.
  #
  # We wait for the process to actually reach 't' rather than just sleeping after
  # the write, because the MI command is queued and applied asynchronously -- see
  # wait_for_state in common.sh for what goes wrong otherwise.
  echo "Injecting $NUM_PAUSES pauses of ${PAUSE_MS}ms..."
  local pause_s gap_s
  pause_s="$(awk -v ms="$PAUSE_MS" 'BEGIN{printf "%.3f", ms/1000}')"
  gap_s="$(awk -v ms="$GAP_MS" 'BEGIN{printf "%.3f", ms/1000}')"

  local i landed=0
  for i in $(seq 1 "$NUM_PAUSES"); do
    if ! kill -0 "$app_pid" 2>/dev/null; then
      echo ""
      echo "  app finished after $((i-1)) pauses (raise DURATION_SEC for more)"
      break
    fi

    mi "-exec-interrupt"
    if ! wait_for_state "$app_pid" t 10; then
      echo ""
      echo "  WARNING: pause $i -- the process never stopped; skipping"
      continue
    fi

    # It is stopped. Hold it here: this is the pause the app must not perceive.
    sleep "$pause_s"

    mi "-record-time-and-continue"
    if ! wait_for_state "$app_pid" '!t' 10; then
      echo ""
      echo "  WARNING: pause $i -- the process never resumed; skipping the rest"
      break
    fi
    landed=$((landed + 1))

    sleep "$gap_s"
    printf '\r  pause %d/%d' "$i" "$NUM_PAUSES"
  done
  echo ""
  [[ "$landed" -gt 0 ]] || die "no pause ever took effect -- the app was never stopped.
  ddb log: $LOG_DIR/ddb.log"
  echo "  $landed pauses landed"

  # ── wait for the app to finish on its own ────────────────────────────────
  # It stops after DURATION_SEC of real time. Give it that long again as slack,
  # since the pauses do not count against its own rdtsc-based clock... they do,
  # actually -- rdtsc keeps running while it is stopped -- so it can only finish
  # early, never late. The slack is for the flush of the last samples.
  echo "Waiting for the app to finish..."
  local waited=0
  while kill -0 "$app_pid" 2>/dev/null; do
    sleep 1
    waited=$((waited + 1))
    if [[ "$waited" -gt $((DURATION_SEC + 60)) ]]; then
      die "app did not exit within $((DURATION_SEC + 60))s. See $app_log"
    fi
  done
  wait "$app_pid" 2>/dev/null || true

  if grep -q 'WARNING: dropped' "$app_log"; then
    echo "  WARNING: the app dropped samples (ring buffer full):"
    grep 'WARNING: dropped' "$app_log" | sed 's/^/    /'
  fi

  [[ -s "$samples" ]] || die "the app produced no samples at $samples. See $app_log"
  echo "  $(( $(wc -l < "$samples") - 1 )) samples -> $samples"

  reset_local

  # Hand the path back to the caller.
  LAST_SAMPLES="$samples"
}

trap cleanup EXIT INT TERM

declare -a produced=()
if [[ "$MODE" == "both" ]]; then
  run_one baseline; produced+=("$LAST_SAMPLES")
  run_one faketime; produced+=("$LAST_SAMPLES")
else
  run_one "$MODE";  produced+=("$LAST_SAMPLES")
fi

# ── analysis ────────────────────────────────────────────────────────────────
echo ""
echo "############################################################"
echo "# Results"
echo "############################################################"
python3 "$EXP_DIR/analyze.py" --pause-ms "$PAUSE_MS" "${produced[@]}"
