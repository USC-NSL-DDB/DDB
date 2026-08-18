#!/usr/bin/env bash
set -euo pipefail

api_docs_tools_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
api_docs_site="${api_docs_tools_root}/docs-site"

command -v npm >/dev/null || {
  echo "npm is required to install the locked API documentation tools" >&2
  exit 2
}
command -v sha256sum >/dev/null || {
  echo "sha256sum is required to verify the API documentation tool lockfile" >&2
  exit 2
}

api_docs_manifest="${api_docs_site}/package.json"
api_docs_lock="${api_docs_site}/package-lock.json"
api_docs_stamp="${api_docs_site}/node_modules/.ddb-package-lock.sha256"
api_docs_lock_signature="$(sha256sum "${api_docs_manifest}" "${api_docs_lock}")"
api_docs_tools_current=true
for api_docs_tool in redocly asyncapi; do
  if [[ ! -x "${api_docs_site}/node_modules/.bin/${api_docs_tool}" ]]; then
    api_docs_tools_current=false
  fi
done
if [[ ! -r "${api_docs_stamp}" ]] ||
  [[ "$(<"${api_docs_stamp}")" != "${api_docs_lock_signature}" ]]; then
  api_docs_tools_current=false
fi

if [[ "${api_docs_tools_current}" != true ]]; then
  npm ci --prefix "${api_docs_site}" --ignore-scripts
  printf '%s\n' "${api_docs_lock_signature}" >"${api_docs_stamp}"
fi
