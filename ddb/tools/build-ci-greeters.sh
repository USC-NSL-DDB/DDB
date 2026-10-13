#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="${repository_root}/ddb/core/tests/fixtures/greeter"

if [[ ! -f "${repository_root}/fwks/grpc/third_party/protobuf/CMakeLists.txt" ]]; then
  echo 'Initialize fwks/grpc and its abseil-cpp, protobuf and re2 submodules first.' >&2
  exit 1
fi

cmake -S "${fixture}" -B "${fixture}/build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Debug \
  -DCMAKE_C_COMPILER_LAUNCHER=ccache \
  -DCMAKE_CXX_COMPILER_LAUNCHER=ccache
cmake --build "${fixture}/build" --target greeter_client greeter_server \
  --parallel "${DDB_CI_BUILD_JOBS:-2}"
