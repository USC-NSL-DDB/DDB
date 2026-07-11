#!/bin/bash
#
# Build everything the perceived-time-gap experiment needs:
#   1. libfaketime  -- the DDB-shipped copy in this repo, not a packaged one
#   2. ddb          -- cargo release build (it embeds the gdb extension, so a
#                      stale binary ships a stale runtime-gdb.py)
#   3. faketime_pause -- the application under test
#   4. matplotlib     -- only for the figure; optional
#
# Usage: ./build_all.sh

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

echo "=== 1/4 libfaketime ==="
# Must be the copy in this repo. DDB's fixes to it (futex handling, immature
# wakeups from nanosleep/clock_nanosleep, pthread_cond_clockwait interposition)
# are not in any upstream release, and without them a process that is sleeping
# when the offset moves can wake up early or hang.
if ! command -v gcc >/dev/null; then
  die "gcc not found; install build-essential"
fi
make -C "$LIBFAKETIME_DIR/src" all
[[ -f "$LIBFAKETIME_SO" ]] || die "libfaketime build produced no $LIBFAKETIME_SO"
echo "  $LIBFAKETIME_SO"

echo ""
echo "=== 2/4 ddb ==="
command -v cargo >/dev/null || die "cargo not found; install the Rust toolchain (https://rustup.rs)"
cargo build --release --manifest-path "$REPO_ROOT/ddb/Cargo.toml"
echo "  $DDB_BIN"

echo ""
echo "=== 3/4 faketime_pause (application under test) ==="
# The connector's service reporter speaks MQTT through paho.
#
# `grep -c ... >/dev/null` rather than `grep -q`: -q exits on the first match and
# closes the pipe, ldconfig dies of SIGPIPE, and `set -o pipefail` then reports the
# whole pipeline as failed -- intermittently, depending on whether ldconfig had
# finished writing. -c reads its input to the end, so there is no race.
if ! ldconfig -p 2>/dev/null | grep -c 'libpaho-mqtt3c\.so' >/dev/null; then
  die "libpaho-mqtt3c not found. The DDB connector needs it. Build it with:

    git clone https://github.com/eclipse/paho.mqtt.c.git /tmp/paho.mqtt.c
    make -C /tmp/paho.mqtt.c && sudo make -C /tmp/paho.mqtt.c install && sudo ldconfig

  (or run $REPO_ROOT/scripts/setup.sh, which does this among other things)"
fi
make -C "$EXP_DIR"
echo "  $APP_BIN"

echo ""
echo "=== 4/4 analysis deps (matplotlib) ==="
# Only needed for the figure -- analyze.py prints its table either way, so a
# failure here is a warning, not an error.
#
# Installed into ./.pydeps rather than $HOME: the root filesystem on the CloudLab
# image this ships with is 16G and typically has a couple of hundred MB free, so a
# plain `pip install --user` runs out of space partway through numpy. TMPDIR moves
# pip's unpack scratch off the small disk for the same reason.
if python3 -c 'import matplotlib' 2>/dev/null; then
  echo "  matplotlib already available"
elif [[ -d "$EXP_DIR/.pydeps" ]] && PYTHONPATH="$EXP_DIR/.pydeps" python3 -c 'import matplotlib' 2>/dev/null; then
  echo "  matplotlib already vendored in $EXP_DIR/.pydeps"
else
  export TMPDIR="${TMPDIR:-/mnt/local/tmp}"
  mkdir -p "$TMPDIR" "$EXP_DIR/.pydeps"
  if pip install --quiet --no-cache-dir --target "$EXP_DIR/.pydeps" matplotlib; then
    echo "  matplotlib -> $EXP_DIR/.pydeps"
  else
    echo "  WARNING: could not install matplotlib. analyze.py will still print its"
    echo "           table of results, but will not draw the figure."
  fi
fi

echo ""
echo "Build complete. Next:"
echo "  ./run_experiment.sh              # faketime on: the mechanism under test"
echo "  ./run_experiment.sh --mode baseline   # faketime off: the control"
