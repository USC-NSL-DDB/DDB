#!/bin/bash

CLUSTER_FILE="${1:-cluster.txt}"
MASTER_IP="10.10.1.1"
SERVER_URL="https://${MASTER_IP}:6443"

if [[ ! -f "$CLUSTER_FILE" ]]; then
  echo "Error: cluster file '$CLUSTER_FILE' not found" >&2
  exit 1
fi

NODE_TOKEN=$(sudo cat /var/lib/rancher/k3s/server/node-token 2>/dev/null)
if [[ -z "$NODE_TOKEN" ]]; then
  echo "Error: failed to read node token from master" >&2
  exit 1
fi

echo "Server URL: $SERVER_URL"
echo "Token:      ${NODE_TOKEN:0:20}..."
echo ""

pids=()
nodes=()

while IFS= read -r ip || [[ -n "$ip" ]]; do
  # skip blank lines and comments
  [[ -z "$ip" || "$ip" == \#* ]] && continue

  # skip master node
  [[ "$ip" == "$MASTER_IP" ]] && continue

  echo "[*] Starting k3s agent on $ip ..."
  ssh -o StrictHostKeyChecking=no "$ip" \
    "sudo systemctl stop k3s 2>/dev/null; sudo killall k3s-server k3s-agent k3s 2>/dev/null; sleep 1; nohup sudo k3s agent --server ${SERVER_URL} --token ${NODE_TOKEN} > /tmp/k3s-agent.log 2>&1 &" &
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
