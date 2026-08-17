#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  echo "usage: $0 <git-base-ref>" >&2
  exit 2
fi

asyncapi_base_ref="$1"
asyncapi_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
asyncapi_repo_prefix="$(git -C "${asyncapi_root}" rev-parse --show-prefix)"
asyncapi_repo_path="${asyncapi_repo_prefix}docs/api/generated/asyncapi-v2.json"
asyncapi_current="${asyncapi_root}/docs/api/generated/asyncapi-v2.json"

if ! git -C "${asyncapi_root}" cat-file -e \
  "${asyncapi_base_ref}:${asyncapi_repo_path}" 2>/dev/null; then
  echo "No AsyncAPI v2 baseline exists at ${asyncapi_base_ref}; establishing the initial baseline."
  exit 0
fi

command -v jq >/dev/null || {
  echo "jq is required for AsyncAPI compatibility checks" >&2
  exit 2
}
command -v npx >/dev/null || {
  echo "npx is required for AsyncAPI compatibility checks" >&2
  exit 2
}

asyncapi_tmp="$(mktemp -d)"
trap 'rm -rf "${asyncapi_tmp}"' EXIT

git -C "${asyncapi_root}" show \
  "${asyncapi_base_ref}:${asyncapi_repo_path}" >"${asyncapi_tmp}/base.json"

# @asyncapi/diff requires a dereferenced document and cannot dereference DDB's
# intentionally recursive Protobuf schema graph. Buf owns payload compatibility;
# this projection lets the official AsyncAPI diff own operations, channels,
# messages, bindings, and DDB stream-semantics extensions without duplicating
# Protobuf compatibility logic.
normalize_asyncapi() {
  jq '
    del(.channels[]["x-ddb-request-schema"])
    | .components.messages[].payload = {"type": "object"}
    | .components.schemas = {}
  ' "$1" >"$2"
}

normalize_asyncapi "${asyncapi_tmp}/base.json" "${asyncapi_tmp}/base-normalized.json"
normalize_asyncapi "${asyncapi_current}" "${asyncapi_tmp}/current-normalized.json"

SUPPRESS_NO_CONFIG_WARNING=true npx --yes @asyncapi/cli@6.0.2 diff \
  "${asyncapi_tmp}/base-normalized.json" \
  "${asyncapi_tmp}/current-normalized.json" \
  --type breaking \
  --format json \
  --overrides "${asyncapi_root}/tools/asyncapi-compatibility-overrides.json"
