#!/bin/bash
#
# Build everything the raft overhead experiment needs, on the node you drive it
# from (node0):
#   1. a real g++-13  (the image ships an experimental 13.0.0 with no <format>)
#   2. paho.mqtt.c    (the DDB connector's MQTT client; raft_node links it)
#   3. the DDB-patched gRPC + the DDB connector headers -> $DEPS_PREFIX
#   4. ddb itself     (cargo)
#   5. raft-lab: submodules, then a Release build of raft_node + tput_remote
#
# Nothing here modifies the raft-lab sources. The build is in-tree at
# $RAFT_DIR/build (which raft-lab's own .gitignore already ignores) and the
# submodule checkout is what raft-lab's setup.sh does anyway, so a fresh clone
# still behaves exactly as its README describes.
#
# Usage: ./build_all.sh [--skip-deps]

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

SKIP_DEPS=0
[[ "${1:-}" == "--skip-deps" ]] && SKIP_DEPS=1

require_raft_dir

# gcc's LTO and cmake write large temporaries; / is 16G on these images.
export TMPDIR="${TMPDIR:-/mnt/local/tmp}"
mkdir -p "$TMPDIR"

echo "=== 1/5 toolchain ==="
# /usr/local/bin/g++-13 (an experimental 13.0.0 snapshot from the image) shadows
# the packaged one on PATH and has no <format>, which raft-lab needs. Test the
# absolute path we actually build with, and install the real one if it is stale.
if ! echo '#include <format>
int main(){ return std::format("{}", 1).size() == 1 ? 0 : 1; }' \
    | "$CXX_BIN" -std=c++20 -x c++ - -o /dev/null 2>/dev/null; then
  echo "  $CXX_BIN cannot compile <format>; installing gcc-13 from ppa:ubuntu-toolchain-r/test"
  sudo add-apt-repository -y ppa:ubuntu-toolchain-r/test
  sudo apt-get update
  sudo apt-get install -y gcc-13 g++-13
  echo '#include <format>
int main(){ return std::format("{}", 1).size() == 1 ? 0 : 1; }' \
    | "$CXX_BIN" -std=c++20 -x c++ - -o /dev/null \
    || die "$CXX_BIN still cannot compile <format>"
fi
echo "  $CXX_BIN -> $("$CXX_BIN" --version | head -1)"
# Note we deliberately do NOT run raft-lab's scripts/install_gcc-13.sh: it points
# the system-wide `gcc`/`g++` alternatives at 13, which would change the compiler
# out from under the other experiments on this machine. We use absolute paths.

if [[ "$SKIP_DEPS" -eq 0 ]]; then
  echo ""
  echo "=== 2/5 paho.mqtt.c ==="
  # raft_node links paho-mqtt3c unconditionally (the DDB connector reports itself
  # over MQTT), so it is needed even for the no-debugger baseline.
  if ldconfig -p | grep -q 'libpaho-mqtt3c\.so'; then
    echo "  already installed ($(ldconfig -p | awk '/libpaho-mqtt3c\.so /{print $NF; exit}'))"
  else
    sudo apt-get install -y build-essential autoconf libtool pkg-config libssl-dev
    rm -rf "$TMPDIR/paho.mqtt.c"
    git clone -q https://github.com/eclipse/paho.mqtt.c.git "$TMPDIR/paho.mqtt.c"
    make -C "$TMPDIR/paho.mqtt.c" -j"$(nproc)" >/dev/null
    sudo make -C "$TMPDIR/paho.mqtt.c" install >/dev/null
    sudo ldconfig
    echo "  installed to /usr/local/lib"
  fi

  echo ""
  echo "=== 3/5 DDB-patched gRPC + connector headers -> $DEPS_PREFIX ==="
  # raft-lab's CMakeLists hard-codes grpc_prefix=$HOME/.local, but $HOME is on the
  # 16G root filesystem and gRPC does not fit. We install to $DEPS_PREFIX on
  # /mnt/local instead and override CMAKE_PREFIX_PATH at configure time (a -D on
  # the command line pre-seeds the cache, so their `set(... CACHE ...)` is a no-op).
  # No raft-lab file is touched.
  #
  # gRPC comes from DDB's own fwks/grpc submodule -- it is the DDB-patched fork
  # (grpc-ddb), which is what DDB's `Framework: grpc` adapter expects, and it is
  # already vendored here so there is nothing extra to clone.
  if [[ -f "$DEPS_PREFIX/lib/cmake/grpc/gRPCConfig.cmake" ]]; then
    echo "  gRPC already installed."
  else
    [[ -f "$REPO_ROOT/fwks/grpc/CMakeLists.txt" ]] \
      || die "DDB's fwks/grpc submodule is empty. Run:
  git -C $REPO_ROOT submodule update --init --recursive fwks/grpc"
    cmake -S "$REPO_ROOT/fwks/grpc" -B "$TMPDIR/build-grpc" -G Ninja \
      -DCMAKE_BUILD_TYPE=RelWithDebInfo \
      -DgRPC_INSTALL=ON -DgRPC_BUILD_TESTS=OFF \
      -DCMAKE_INSTALL_PREFIX="$DEPS_PREFIX" \
      -DCMAKE_C_COMPILER="$CC_BIN" -DCMAKE_CXX_COMPILER="$CXX_BIN" >/dev/null
    echo "  building gRPC (this takes 15-30 min the first time)..."
    cmake --build "$TMPDIR/build-grpc" --target install -j "$(nproc)" >/dev/null
  fi

  echo ""
  echo "=== 4/5 ddb ==="
  if [[ -x "$DDB_BIN" ]]; then
    echo "  already built."
  else
    command -v cargo >/dev/null || die "cargo not found. Install rust:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source \$HOME/.cargo/env"
    cargo build --release --manifest-path "$REPO_ROOT/ddb/Cargo.toml"
  fi
else
  echo ""
  echo "=== 2-4/5 paho / gRPC / ddb skipped (--skip-deps) ==="
fi

# The connector headers go into the SAME prefix as gRPC, so raft_node's
# `#include <ddb/integration.hpp>` resolves off the include path gRPC already
# adds. Always refreshed: they must match the ddb build in this tree.
make -C "$CONNECTOR_DIR" PREFIX="$DEPS_PREFIX" install >/dev/null
[[ -f "$DEPS_PREFIX/include/ddb/integration.hpp" ]] || die "connector headers not installed"
echo "  DDB connector headers -> $DEPS_PREFIX/include/ddb"

echo ""
echo "=== 5/5 raft-lab ==="
# spdlog + googletest are git submodules of raft-lab; this is exactly what
# raft-lab's own setup.sh does. It populates libs/, it does not change any code.
if [[ ! -f "$RAFT_DIR/libs/spdlog/CMakeLists.txt" ]]; then
  echo "  fetching raft-lab submodules..."
  git -C "$RAFT_DIR" submodule update --init --recursive --jobs "$(nproc)" \
    libs/spdlog libs/googletest
fi

# Release is "-O3 -g" in raft-lab's CMakeLists: optimised (so the throughput
# number means something) but with symbols (so gdb/DDB have something to attach
# to). Their default is Debug/-O0, which would understate every mode.
#
# -static-libstdc++ so the binary we ship to node1..3 does not depend on the
# gcc-13 runtime being installed there too.
cmake -S "$RAFT_DIR" -B "$RAFT_DIR/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_C_COMPILER="$CC_BIN" -DCMAKE_CXX_COMPILER="$CXX_BIN" \
  -DCMAKE_PREFIX_PATH="$DEPS_PREFIX" \
  -DCMAKE_CXX_FLAGS="-I$DEPS_PREFIX/include" \
  -DCMAKE_EXE_LINKER_FLAGS="-static-libstdc++ -static-libgcc" >/dev/null \
  || die "cmake configure failed for $RAFT_DIR"

cmake --build "$RAFT_DIR/build" --target raft_node tput_remote -j "$(nproc)" \
  || die "failed to build raft_node / tput_remote"

echo ""
echo "=== Build complete ==="
for f in "$RAFT_NODE_BIN" "$TPUT_BIN" "$DDB_BIN"; do
  [[ -x "$f" ]] && echo "  ok      $f" || echo "  MISSING $f"
done
echo ""
echo "Next: ./setup_nodes.sh     (replicate the binaries to node1..node3)"
