#!/usr/bin/env bash
set -euo pipefail

api_release_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${api_release_root}"

cargo run -p ddb-api-codegen -- --check
npx --yes @redocly/cli@2.46.1 lint docs/api/generated/openapi-v2.json
npx --yes @asyncapi/cli@6.0.2 validate \
  docs/api/generated/asyncapi-v2.json --diagnostics-format json
cargo test -p ddb-api-types --all-targets
cargo test -p ddb-api-client --all-targets
cargo test -p ddb-api-grpc --all-targets
cargo test -p ddb-api-conformance --all-targets
cargo test -p ddb-api-extension --all-targets
cargo test -p ddb-sample-extension --all-targets
cargo test -p ddb --test api_v2_spec_conformance
cargo clippy -p ddb-api-types --all-targets --no-deps -- -D warnings
cargo clippy -p ddb-api-client --all-targets --all-features --no-deps -- -D warnings
cargo clippy -p ddb-api-grpc --all-targets --all-features --no-deps -- -D warnings
cargo clippy -p ddb-api-conformance --all-targets --no-deps -- -D warnings
cargo clippy -p ddb-api-extension --all-targets --no-deps -- -D warnings
cargo clippy -p ddb-sample-extension --all-targets --no-deps -- -D warnings

npm ci --prefix sdk/typescript --ignore-scripts
npm run --prefix sdk/typescript check
npm test --prefix sdk/typescript
PYTHONPATH=sdk/python/src python3 -m compileall -q \
  sdk/python/src sdk/python/tests sdk/python/examples
PYTHONPATH=sdk/python/src python3 -m unittest discover -s sdk/python/tests -v

package_api_crates() {
  cargo package -p ddb-api-types --allow-dirty --no-verify
  cargo package -p ddb-api-client --allow-dirty --no-verify \
    --config 'patch.crates-io.ddb-api-types.path="api-types"'
  cargo package -p ddb-api-grpc --allow-dirty --no-verify \
    --config 'patch.crates-io.ddb-api-types.path="api-types"'
  cargo package -p ddb-api-conformance --allow-dirty --no-verify \
    --config 'patch.crates-io.ddb-api-client.path="api-client"' \
    --config 'patch.crates-io.ddb-api-types.path="api-types"'
  cargo package -p ddb-api-extension --allow-dirty --no-verify \
    --config 'patch.crates-io.ddb-api-types.path="api-types"'
}

package_hashes() {
  sha256sum \
    target/package/ddb-api-types-0.1.0.crate \
    target/package/ddb-api-client-0.1.0.crate \
    target/package/ddb-api-grpc-0.1.0.crate \
    target/package/ddb-api-conformance-0.1.0.crate \
    target/package/ddb-api-extension-0.1.0.crate
}

package_api_crates
api_release_first_hashes="$(package_hashes)"
package_api_crates
api_release_second_hashes="$(package_hashes)"

if [[ "${api_release_first_hashes}" != "${api_release_second_hashes}" ]]; then
  echo "API crate packages are not byte-for-byte reproducible" >&2
  diff \
    <(printf '%s\n' "${api_release_first_hashes}") \
    <(printf '%s\n' "${api_release_second_hashes}") >&2 || true
  exit 1
fi

api_release_tmp="$(mktemp -d)"
trap 'rm -rf "${api_release_tmp}"' EXIT
mkdir -p \
  "${api_release_tmp}/typescript-first" \
  "${api_release_tmp}/typescript-second" \
  "${api_release_tmp}/python-first" \
  "${api_release_tmp}/python-second"

npm pack ./sdk/typescript \
  --pack-destination "${api_release_tmp}/typescript-first" >/dev/null
npm pack ./sdk/typescript \
  --pack-destination "${api_release_tmp}/typescript-second" >/dev/null
SOURCE_DATE_EPOCH=1704067200 python3 -m pip wheel ./sdk/python --no-deps \
  --wheel-dir "${api_release_tmp}/python-first" >/dev/null
SOURCE_DATE_EPOCH=1704067200 python3 -m pip wheel ./sdk/python --no-deps \
  --wheel-dir "${api_release_tmp}/python-second" >/dev/null

typescript_package="ddb-debugger-api-client-0.1.0.tgz"
python_package="ddb_api_client-0.1.0-py3-none-any.whl"
if ! cmp -s \
  "${api_release_tmp}/typescript-first/${typescript_package}" \
  "${api_release_tmp}/typescript-second/${typescript_package}"; then
  echo "TypeScript SDK packages are not byte-for-byte reproducible" >&2
  exit 1
fi
if ! cmp -s \
  "${api_release_tmp}/python-first/${python_package}" \
  "${api_release_tmp}/python-second/${python_package}"; then
  echo "Python SDK packages are not byte-for-byte reproducible" >&2
  exit 1
fi

printf '%s\n' "${api_release_second_hashes}"
sha256sum \
  "${api_release_tmp}/typescript-second/${typescript_package}" \
  "${api_release_tmp}/python-second/${python_package}"
echo "API release dry run passed; no artifacts were published"
