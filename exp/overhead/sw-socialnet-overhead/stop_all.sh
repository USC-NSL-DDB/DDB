#!/bin/bash
#
# Tear down everything a run of this experiment created, in one command.
#
#   default:    kill DDB + benchmark processes on the head node, then delete
#               the socialnet app, its services/HPAs, and the ssh-gateway from
#               the cluster. The k3s cluster itself stays up, so the next
#               ./setup_experiment.sh --skip-build restores the app in minutes.
#   --cluster:  additionally stop k3s everywhere: k3s-killall.sh on every
#               worker (kills the agent, all containers, and flushes the CNI
#               iptables state) and stop the k3s server on the master.
#
# Deliberately KEPT (they are installation or data, not run state): the k3s /
# Go / docker binaries, docker image cache, built socialnet binaries,
# ~/.kube/config, ~/ddb-tmp logs, and the sysctl tuning.
#
# Safe to run anytime, in any state; every step is best-effort.
#
# Usage: ./stop_all.sh [--cluster]

set -uo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

CLUSTER_FILE="$EXP_DIR/cluster.txt"
STOP_CLUSTER=0
[[ "${1:-}" == "--cluster" ]] && STOP_CLUSTER=1

# Bounded kubectl so a dead API server cannot hang the teardown.
kc() { kubectl --request-timeout=5s "$@"; }

# ─── 1. Local processes on the head node ─────────────────────────────────────
echo "=== Stopping local DDB / benchmark processes ==="
# ddb first, politely: on SIGTERM it detaches its gdb sessions, un-freezing the
# app pods. Escalate only if it lingers.
if pgrep -x ddb >/dev/null; then
  pkill -x ddb 2>/dev/null
  for _ in 1 2 3 4 5; do pgrep -x ddb >/dev/null || break; sleep 1; done
  pkill -9 -x ddb 2>/dev/null
  echo "  ddb stopped"
fi
pkill -f 'client\.out'      2>/dev/null && echo "  benchmark client stopped"
pkill -f 'init_social\.out' 2>/dev/null && echo "  seeder stopped"
echo "  done"

# ─── 2. Cluster objects (app + gateway) ──────────────────────────────────────
echo ""
echo "=== Removing the app + ssh-gateway from the cluster ==="
if [[ -r "$HOME/.kube/config" ]] && KUBECONFIG="$HOME/.kube/config" kc get nodes >/dev/null 2>&1; then
  export KUBECONFIG="$HOME/.kube/config"
  # Deleting the pods also kills any gdb still running in the ssh-debugger
  # ephemeral containers (a ptrace-stopped inferior still honours SIGKILL).
  # NB: collect names into arrays -- `kc` is a shell function, so it cannot be
  # the command of an xargs pipeline (xargs can only exec real programs).
  mapfile -t objs < <(kc get deployments,hpa -n "$NAMESPACE" -o name 2>/dev/null)
  mapfile -t -O "${#objs[@]}" objs < <(kc get svc -n "$NAMESPACE" -o name 2>/dev/null \
    | grep -v '^service/kubernetes$')
  if [[ ${#objs[@]} -gt 0 ]]; then
    echo "  deleting ${#objs[@]} app objects (deployments/hpa/services)"
    kc delete -n "$NAMESPACE" "${objs[@]}" --wait=false >/dev/null 2>&1
  fi
  kc delete pod ssh-gateway -n "$NAMESPACE" --ignore-not-found --wait=false >/dev/null 2>&1
  # Bounded wait so we report reality, but a stuck pod cannot wedge teardown.
  for _ in $(seq 1 30); do
    n=$(kc get pods -n "$NAMESPACE" --no-headers 2>/dev/null | wc -l)
    [[ "$n" -eq 0 ]] && break
    sleep 2
  done
  n=$(kc get pods -n "$NAMESPACE" --no-headers 2>/dev/null | wc -l)
  if [[ "$n" -eq 0 ]]; then
    echo "  all app pods gone"
  else
    echo "  WARNING: $n pod(s) still terminating (they will finish on their own)"
  fi
else
  echo "  cluster not reachable; skipping (nothing to delete, or use --cluster on the nodes)"
fi

# ─── 3. Optionally: the k3s cluster itself ───────────────────────────────────
if [[ "$STOP_CLUSTER" -eq 1 ]]; then
  echo ""
  echo "=== Stopping k3s on every node (--cluster) ==="
  if [[ -f "$CLUSTER_FILE" ]]; then
    while IFS= read -r ip || [[ -n "$ip" ]]; do
      [[ -z "$ip" || "$ip" == \#* || "$ip" == "$MASTER_IP" ]] && continue
      echo "  worker $ip: k3s-killall"
      ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout=8 "$ip" \
        'sudo /usr/local/bin/k3s-killall.sh >/dev/null 2>&1; sudo rm -f /tmp/k3s-agent.log' 2>/dev/null \
        || echo "    (unreachable, skipped)"
    done <"$CLUSTER_FILE"
  else
    echo "  WARNING: $CLUSTER_FILE not found; workers not touched" >&2
  fi
  echo "  master: stopping k3s server"
  sudo systemctl stop k3s 2>/dev/null
  sudo /usr/local/bin/k3s-killall.sh >/dev/null 2>&1
  echo "  done. Restore everything with: ./setup_experiment.sh --skip-build"
fi

echo ""
echo "Stopped."
