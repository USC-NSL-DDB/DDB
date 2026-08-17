#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 ARCHIVE.tar.gz" >&2
  exit 2
fi

smoke_archive="$1"
smoke_tmp="$(mktemp -d)"
serve_pid=""
cleanup() {
  if [[ -n "${serve_pid}" ]] && kill -0 "${serve_pid}" 2>/dev/null; then
    kill -TERM "${serve_pid}" 2>/dev/null || true
    wait "${serve_pid}" 2>/dev/null || true
  fi
  rm -rf "${smoke_tmp}"
}
trap cleanup EXIT
tar -xzf "${smoke_archive}" -C "${smoke_tmp}"
mapfile -t smoke_roots < <(find "${smoke_tmp}" -mindepth 1 -maxdepth 1 -type d)
if [[ ${#smoke_roots[@]} -ne 1 ]]; then
  echo "archive must contain exactly one top-level directory" >&2
  exit 1
fi
smoke_root="${smoke_roots[0]}"
ddb_bin="${smoke_root}/bin/ddb"
tui_bin="${smoke_root}/bin/ddb-tui"
mock_config="${smoke_root}/examples/managed/mock.yaml"

[[ -x "${ddb_bin}" && -x "${tui_bin}" && -f "${mock_config}" ]]
grep -q '"supported_api_versions": \["v2"\]' "${smoke_root}/manifest.json"

PATH='' "${ddb_bin}" tui --help >/dev/null
PATH='' "${tui_bin}" --ddb-path "${ddb_bin}" --help >/dev/null

token_file="${smoke_tmp}/tokens.json"
startup_report="${smoke_tmp}/startup.json"
printf '%s\n' '{"tokens":[{"token":"release-smoke-admin-token-0000000000000000","scope":"admin"}]}' >"${token_file}"
chmod 0600 "${token_file}"
(
  cd "${smoke_root}"
  exec env PATH='' \
    ./bin/ddb serve ./examples/managed/mock.yaml \
    --managed \
    --api-auth-token-file "${token_file}" \
    --startup-report "${startup_report}"
) &
serve_pid=$!
for _ in {1..750}; do
  if [[ -f "${startup_report}" ]]; then
    break
  fi
  if ! kill -0 "${serve_pid}" 2>/dev/null; then
    wait "${serve_pid}" || true
    echo "packaged ddb serve exited before readiness" >&2
    exit 1
  fi
  sleep 0.02
done
grep -q '"status":"ready"' "${startup_report}"
kill -TERM "${serve_pid}"
wait "${serve_pid}"
serve_pid=""

script_bin="$(command -v script || true)"
if [[ -z "${script_bin}" ]]; then
  echo "script(1) is required for packaged TUI smoke tests" >&2
  exit 1
fi
printf -v smoke_root_q '%q' "${smoke_root}"
printf 'q' |
  "${script_bin}" -qefc \
    "cd ${smoke_root_q} && PATH='' exec ./bin/ddb tui ./examples/managed/mock.yaml" \
    /dev/null >/dev/null
printf 'q' |
  "${script_bin}" -qefc \
    "cd ${smoke_root_q} && PATH='' exec ./bin/ddb-tui --ddb-path ./bin/ddb ./examples/managed/mock.yaml" \
    /dev/null >/dev/null

echo "paired DDB/ddb-tui artifact smoke tests passed"
