#!/bin/bash
#
# Build everything this experiment needs, on the node you drive it from:
#   1. DDB connector headers -> ~/.local/include/ddb  (Nu compiles against these)
#   2. caladan (+ the ksched kernel module)
#   3. Nu, with CONFIG_DDB=y  -> libnu.a, bin/ctrl_main, bin/ctrl_proxy
#   4. DDB itself (cargo)
#   5. the socialNetwork app (thrift, json, cpp-jwt, backend, client)
#
# Usage: ./build_all.sh [--skip-socialnet] [--skip-ddb]

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SKIP_SOCIALNET=0
SKIP_DDB=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-socialnet) SKIP_SOCIALNET=1; shift ;;
    --skip-ddb)       SKIP_DDB=1;       shift ;;
    *) die "Unknown option: $1" ;;
  esac
done

# gcc's LTO writes large temporaries; / is small on these images.
export TMPDIR="${TMPDIR:-/mnt/local/tmp}"
mkdir -p "$TMPDIR"

echo "=== 1/5 DDB connector headers ==="
make -C "$CONNECTOR_DIR" install
[[ -f "$HOME/.local/include/ddb/integration.hpp" ]] || die "connector headers not installed"

echo ""
echo "=== 2/5 caladan ==="
if [[ ! -x "$NU_DIR/caladan/iokerneld" ]]; then
  ( cd "$NU_DIR/caladan" && ./build.sh )
else
  echo "iokerneld already built."
fi
# ksched is a kernel module; it must match the running kernel.
if [[ ! -f "$NU_DIR/caladan/ksched/build/ksched.ko" ]]; then
  make -C "$NU_DIR/caladan/ksched"
fi

echo ""
echo "=== 3/5 Nu (CONFIG_DDB=y) ==="
grep -q '^CONFIG_DDB=y' "$NU_DIR/build/config" || die "set CONFIG_DDB=y in $NU_DIR/build/config"
make -C "$NU_DIR" -j"$(nproc)"
[[ -x "$NU_DIR/bin/ctrl_proxy" ]] || die "bin/ctrl_proxy missing -- is CONFIG_DDB=y?"

echo ""
echo "=== 4/5 DDB ==="
if [[ "$SKIP_DDB" -eq 0 && ! -x "$DDB_BIN" ]]; then
  command -v cargo >/dev/null || die "cargo not found. Install rust:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source \$HOME/.cargo/env"
  cargo build --release --manifest-path "$REPO_ROOT/ddb/Cargo.toml"
else
  echo "ddb already built (or skipped)."
fi

echo ""
echo "=== 5/5 socialNetwork app ==="
if [[ "$SKIP_SOCIALNET" -eq 1 ]]; then
  echo "Skipped (--skip-socialnet)."
else
  # The benchmark client lives with the experiment, not the app.
  cp "$NU_DIR/exp/social_net/nu/client.cpp" "$SOCIALNET_DIR/bench/"
  ( cd "$SOCIALNET_DIR" && ./build.sh )
fi

echo ""
echo "=== Build complete ==="
for f in "$NU_DIR/caladan/iokerneld" "$NU_DIR/bin/ctrl_main" "$NU_DIR/bin/ctrl_proxy" \
         "$SOCIALNET_DIR/build/src/main" "$SOCIALNET_DIR/build/bench/client" "$DDB_BIN"; do
  [[ -e "$f" ]] && echo "  ok   $f" || echo "  MISSING $f"
done
