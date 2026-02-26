#!/bin/bash

CLUSTER_FILE="${1:-cluster.txt}"
SCRIPT="install_deps_all.sh"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

HEAD_NODE_INSTALL_SCRIPT="$SCRIPT_DIR/install_deps_head.sh"

if [[ ! -f "$CLUSTER_FILE" ]]; then
  echo "Error: cluster file '$CLUSTER_FILE' not found" >&2
  exit 1
fi

if [[ ! -f "$SCRIPT_DIR/$SCRIPT" ]]; then
  echo "Error: '$SCRIPT' not found in $SCRIPT_DIR" >&2
  exit 1
fi

pids=()
nodes=()

while IFS= read -r ip || [[ -n "$ip" ]]; do
  # skip blank lines and comments
  [[ -z "$ip" || "$ip" == \#* ]] && continue

  echo "[*] Deploying to $ip ..."
  ssh -o StrictHostKeyChecking=no "$ip" 'bash -s' <"$SCRIPT_DIR/$SCRIPT" &
  pids+=($!)
  nodes+=("$ip")
done <"$CLUSTER_FILE"

# wait for all and report results
failed=0
for i in "${!pids[@]}"; do
  if wait "${pids[$i]}"; then
    echo "[OK] ${nodes[$i]}"
  else
    echo "[FAIL] ${nodes[$i]}" >&2
    ((failed++))
  fi
done

echo ""
echo "Done: $((${#nodes[@]} - failed))/${#nodes[@]} succeeded"
[[ $failed -eq 0 ]]

echo "Preparing head node..."

$HEAD_NODE_INSTALL_SCRIPT
