#!/bin/bash
#
# Join every non-master node in cluster.txt to the k3s cluster as an agent.
# Called by setup_experiment.sh; safe to re-run (agents are restarted).

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

CLUSTER_FILE="${1:-$EXP_DIR/cluster.txt}"
SERVER_URL="https://${MASTER_IP}:6443"

if [[ ! -f "$CLUSTER_FILE" ]]; then
  echo "Error: cluster file '$CLUSTER_FILE' not found" >&2
  exit 1
fi

NODE_TOKEN=$(sudo cat /var/lib/rancher/k3s/server/node-token 2>/dev/null)
if [[ -z "$NODE_TOKEN" ]]; then
  echo "Error: failed to read node token from master. Is the k3s server running?" >&2
  exit 1
fi

echo "Server URL: $SERVER_URL"
echo "Token:      ${NODE_TOKEN:0:20}..."
echo ""

pids=()
nodes=()
unreachable=0

# Collect the workers up front. Reading the file inside the loop would let the
# `ssh` below consume the remaining lines from stdin (ssh reads stdin unless
# told not to), so only the first worker would ever be joined.
workers=()
while IFS= read -r ip || [[ -n "$ip" ]]; do
  [[ -z "$ip" || "$ip" == \#* ]] && continue   # blank lines and comments
  [[ "$ip" == "$MASTER_IP" ]] && continue      # master
  workers+=("$ip")
done <"$CLUSTER_FILE"

[[ ${#workers[@]} -gt 0 ]] && echo "Workers: ${workers[*]}" && echo ""

for ip in "${workers[@]}"; do
  # Detect the worker's experiment NIC on the worker itself; don't assume it
  # matches the master's. `-n` keeps ssh off our stdin.
  iface="$(ssh -n -o StrictHostKeyChecking=no "$ip" \
    "ip -o -4 addr show | awk 'index(\$4, \"${MASTER_IP%.*}.\") == 1 { print \$2; exit }'" 2>/dev/null)"
  if [[ -z "$iface" ]]; then
    echo "[FAIL] $ip: unreachable, or no interface carries ${MASTER_IP%.*}.x" >&2
    unreachable=$((unreachable + 1))
    continue
  fi

  agent_args="$(k3s_agent_args "$ip" "$iface")"
  echo "[*] Starting k3s agent on $ip ($agent_args)"

  # Agents run a kubelet too, so they need the same cgroup v1 opt-out as the
  # server, plus an explicit --node-name: several of these hosts are literally
  # named "localhost" and would otherwise all register as the same node.
  ssh -n -o StrictHostKeyChecking=no "$ip" \
    "sudo systemctl stop k3s 2>/dev/null; sudo killall k3s-server k3s-agent k3s 2>/dev/null; sleep 1; nohup sudo k3s agent --server ${SERVER_URL} --token ${NODE_TOKEN} ${agent_args} > /tmp/k3s-agent.log 2>&1 &" &
  pids+=($!)
  nodes+=("$ip")
done

# wait for all and report results
failed=$unreachable
for i in "${!pids[@]}"; do
  if wait "${pids[$i]}"; then
    echo "[OK] ${nodes[$i]}"
  else
    echo "[FAIL] ${nodes[$i]}" >&2
    ((failed++))
  fi
done

echo ""
# Denominator is every worker in cluster.txt, not just the ones we reached --
# otherwise a loop that silently stopped early still reports "1/1 succeeded".
echo "Done: $((${#workers[@]} - failed))/${#workers[@]} succeeded"
[[ $failed -eq 0 ]]
