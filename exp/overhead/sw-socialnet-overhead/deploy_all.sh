#!/bin/bash
#
# Install dependencies (Go, Docker, k3s) on every node in cluster.txt, then
# prepare the head node (weaver-kube + git submodules).
#
# Usage: ./deploy_all.sh [cluster.txt]
#
# After this finishes, log out and back in (or run `newgrp docker`) so your
# docker group membership takes effect, then run ./setup_experiment.sh.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLUSTER_FILE="${1:-$SCRIPT_DIR/cluster.txt}"
SCRIPT="install_deps_all.sh"
HEAD_NODE_INSTALL_SCRIPT="$SCRIPT_DIR/install_deps_head.sh"

# Append ~/.local/bin to PATH in .bashrc on remote nodes (idempotent)
PATH_LINE='export PATH="$HOME/.local/bin:$PATH"'
PATH_FIX="grep -qxF '$PATH_LINE' ~/.bashrc || echo '$PATH_LINE' >> ~/.bashrc"

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
  { cat "$SCRIPT_DIR/$SCRIPT"; echo; echo "$PATH_FIX"; } | ssh -o StrictHostKeyChecking=no "$ip" bash -s &
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

if [[ $failed -ne 0 ]]; then
  echo "Error: dependency install failed on $failed node(s); not preparing head node." >&2
  exit 1
fi

echo ""
echo "Preparing head node..."
"$HEAD_NODE_INSTALL_SCRIPT" || exit 1

echo ""
echo "=== Dependencies installed ==="
echo "Run 'newgrp docker' (or re-login) so docker works without sudo, then:"
echo "  ./setup_experiment.sh"
