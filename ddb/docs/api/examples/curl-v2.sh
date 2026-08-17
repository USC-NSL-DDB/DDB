#!/usr/bin/env bash
set -euo pipefail

ddb_api_url=${DDB_API_URL:-http://127.0.0.1:8080}
ddb_api_token=${DDB_API_TOKEN:-}
rpc_root="${ddb_api_url%/}/api/v2/rpc"

auth_args=()
if [[ -n "${ddb_api_token}" ]]; then
  auth_args=(-H "Authorization: Bearer ${ddb_api_token}")
fi

post_rpc() {
  local service=$1
  local method=$2
  local body=$3
  curl --fail-with-body --silent --show-error \
    "${auth_args[@]}" \
    -H 'Content-Type: application/json' \
    --data "${body}" \
    "${rpc_root}/ddb.api.v2.${service}/${method}"
}

echo 'Capabilities:'
post_rpc DebuggerService GetCapabilities '{}' | jq .

snapshot=$(post_rpc DebuggerService GetSnapshot '{
  "sections": [
    "SNAPSHOT_SECTION_TOPOLOGY",
    "SNAPSHOT_SECTION_EXECUTION",
    "SNAPSHOT_SECTION_BREAKPOINTS",
    "SNAPSHOT_SECTION_PENDING_OPERATIONS",
    "SNAPSHOT_SECTION_CAPABILITIES"
  ]
}')

echo 'Snapshot:'
jq . <<<"${snapshot}"

cursor=$(jq -c '.snapshot.stateEventCursor' <<<"${snapshot}")
subscribe_body=$(jq -cn --argjson cursor "${cursor}" '{afterCursor: $cursor}')

echo 'State events (Ctrl-C to stop; blank lines are heartbeats):'
curl --fail-with-body --no-buffer --silent --show-error \
  "${auth_args[@]}" \
  -H 'Content-Type: application/json' \
  --data "${subscribe_body}" \
  "${rpc_root}/ddb.api.v2.DdbEventService/SubscribeStateEvents"
