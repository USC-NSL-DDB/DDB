#!/usr/bin/env bash
#
# scale_cluster.sh — Add/remove worker nodes for scaling experiments
#
# Cordon+drain to remove workers, uncordon+redistribute to add them back.
# After scaling, pods are redistributed and you must re-seed data.
#
# Usage:
#   ./scale_cluster.sh status                  # show node & pod state
#   ./scale_cluster.sh set 2                   # keep only 2 workers (removes highest-numbered first)
#   ./scale_cluster.sh set 4                   # restore to all 4 workers
#   ./scale_cluster.sh remove node3 node4      # remove specific nodes
#   ./scale_cluster.sh add node3 node4         # add back specific nodes
#   ./scale_cluster.sh add all                 # restore all workers
#
# After any scaling operation, re-seed and (optionally) re-inject DDB sidecars:
#   ./seed_data.sh
#   ./setup_ddb.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# All worker nodes in order (node0 is master, never touched)
WORKER_NODES=(
    "node1.serviceweaver.flashburst-pg0.utah.cloudlab.us"
    "node2.serviceweaver.flashburst-pg0.utah.cloudlab.us"
    "node3.serviceweaver.flashburst-pg0.utah.cloudlab.us"
    "node4.serviceweaver.flashburst-pg0.utah.cloudlab.us"
)
WORKER_SHORT=(node1 node2 node3 node4)
NUM_WORKERS=${#WORKER_NODES[@]}

# ─── Helpers ─────────────────────────────────────────────────────────────────

usage() {
    echo "Usage:"
    echo "  $0 status                    Show current node and pod state"
    echo "  $0 set N                     Scale to N workers (1-4)"
    echo "  $0 remove node3 [node4 ...]  Remove specific workers"
    echo "  $0 add node3 [node4 ...]     Add back specific workers"
    echo "  $0 add all                   Restore all workers"
    exit 1
}

# Resolve short name (node3) to full k8s name
resolve_node() {
    local short="$1"
    for i in "${!WORKER_SHORT[@]}"; do
        if [[ "${WORKER_SHORT[$i]}" == "$short" ]]; then
            echo "${WORKER_NODES[$i]}"
            return 0
        fi
    done
    # Try as-is (maybe they passed the full name)
    if kubectl get node "$short" &>/dev/null; then
        echo "$short"
        return 0
    fi
    echo "ERROR: Unknown node '$short'. Valid workers: ${WORKER_SHORT[*]}" >&2
    return 1
}

# Check if a node is cordoned
is_cordoned() {
    kubectl get node "$1" -o jsonpath='{.spec.unschedulable}' 2>/dev/null
}

# Count active (non-cordoned) workers
count_active_workers() {
    local count=0
    for node in "${WORKER_NODES[@]}"; do
        if [[ "$(is_cordoned "$node")" != "true" ]]; then
            count=$((count + 1))
        fi
    done
    echo "$count"
}

# ─── Status ──────────────────────────────────────────────────────────────────

show_status() {
    echo "=== Cluster Node Status ==="
    local active=0
    local cordoned=0
    for i in "${!WORKER_NODES[@]}"; do
        local node="${WORKER_NODES[$i]}"
        local short="${WORKER_SHORT[$i]}"
        if [[ "$(is_cordoned "$node")" == "true" ]]; then
            echo "  ✗ $short ($node) — CORDONED (excluded)"
            cordoned=$((cordoned + 1))
        else
            local pod_count
            pod_count=$(kubectl get pods --field-selector="spec.nodeName=$node" --no-headers 2>/dev/null | wc -l)
            echo "  ✓ $short ($node) — active, $pod_count pods"
            active=$((active + 1))
        fi
    done
    echo ""
    echo "Active workers: $active / $NUM_WORKERS"
    echo ""
    echo "=== Pod Distribution ==="
    kubectl get pods -o wide --no-headers -n default 2>/dev/null \
        | grep -v ssh-gateway \
        | awk '{printf "  %-60s %s\n", $1, $7}'
}

# ─── Wait for pods ───────────────────────────────────────────────────────────

wait_for_pods() {
    echo "Waiting for all pods to be Ready..."
    for i in $(seq 1 60); do
        local not_ready
        not_ready=$(kubectl get pods -n default --no-headers 2>/dev/null \
            | grep -v "ssh-gateway" \
            | grep -v "1/1.*Running" \
            | wc -l)
        if [[ "$not_ready" -eq 0 ]]; then
            echo "All app pods Ready."
            return 0
        fi
        echo "  $not_ready pod(s) not ready... ($i/60)"
        sleep 5
    done
    echo "WARNING: some pods still not ready after 5 minutes" >&2
    return 1
}

# ─── Remove nodes ────────────────────────────────────────────────────────────

remove_nodes() {
    local nodes=("$@")
    local removed=0

    for node in "${nodes[@]}"; do
        if [[ "$(is_cordoned "$node")" == "true" ]]; then
            echo "  $node already cordoned, skipping"
            continue
        fi
        echo "  Cordoning $node..."
        kubectl cordon "$node" >/dev/null
        echo "  Draining $node (evicting pods)..."
        kubectl drain "$node" --ignore-daemonsets --delete-emptydir-data --timeout=120s 2>&1 \
            | grep -E "^(node|evict)" || true
        removed=$((removed + 1))
    done

    if [[ "$removed" -gt 0 ]]; then
        echo ""
        wait_for_pods
    fi
}

# ─── Add nodes ───────────────────────────────────────────────────────────────

add_nodes() {
    local nodes=("$@")
    local added=0

    for node in "${nodes[@]}"; do
        if [[ "$(is_cordoned "$node")" != "true" ]]; then
            echo "  $node already active, skipping"
            continue
        fi
        echo "  Uncordoning $node..."
        kubectl uncordon "$node" >/dev/null
        added=$((added + 1))
    done

    if [[ "$added" -gt 0 ]]; then
        echo ""
        echo "Restarting deployments to redistribute pods across all active nodes..."
        kubectl get deployments -n default -o name \
            | grep -v ssh-gateway \
            | xargs -I {} kubectl rollout restart {} -n default 2>/dev/null
        echo ""
        wait_for_pods
    fi
}

# ─── Post-scale summary ─────────────────────────────────────────────────────

post_scale_summary() {
    echo ""
    show_status
    echo ""
    echo "=== Next Steps ==="
    echo "  1. Re-seed data:        ./seed_data.sh"
    echo "  2. (If using DDB):      ./setup_ddb.sh"
    echo "  3. Run benchmark:       ./run_benchmark.sh"
}

# ─── Main ────────────────────────────────────────────────────────────────────

[[ $# -lt 1 ]] && usage

CMD="$1"
shift

case "$CMD" in
    status)
        show_status
        ;;

    set)
        [[ $# -ne 1 ]] && { echo "Usage: $0 set N (where N is 1-$NUM_WORKERS)"; exit 1; }
        TARGET="$1"
        if [[ "$TARGET" -lt 1 || "$TARGET" -gt "$NUM_WORKERS" ]]; then
            echo "ERROR: N must be between 1 and $NUM_WORKERS"
            exit 1
        fi

        current=$(count_active_workers)
        echo "Current active workers: $current, target: $TARGET"

        if [[ "$current" -eq "$TARGET" ]]; then
            echo "Already at $TARGET workers. Nothing to do."
            exit 0
        fi

        if [[ "$TARGET" -lt "$current" ]]; then
            # Remove workers from highest-numbered first
            to_remove=()
            for (( i=NUM_WORKERS-1; i>=0; i-- )); do
                if [[ "$(is_cordoned "${WORKER_NODES[$i]}")" != "true" ]]; then
                    to_remove+=("${WORKER_NODES[$i]}")
                fi
                if [[ ${#to_remove[@]} -ge $((current - TARGET)) ]]; then
                    break
                fi
            done
            echo "Removing ${#to_remove[@]} worker(s): ${to_remove[*]}"
            echo ""
            remove_nodes "${to_remove[@]}"
        else
            # Add workers back from lowest-numbered first
            to_add=()
            for i in "${!WORKER_NODES[@]}"; do
                if [[ "$(is_cordoned "${WORKER_NODES[$i]}")" == "true" ]]; then
                    to_add+=("${WORKER_NODES[$i]}")
                fi
                if [[ ${#to_add[@]} -ge $((TARGET - current)) ]]; then
                    break
                fi
            done
            echo "Adding ${#to_add[@]} worker(s): ${to_add[*]}"
            echo ""
            add_nodes "${to_add[@]}"
        fi

        post_scale_summary
        ;;

    remove)
        [[ $# -lt 1 ]] && { echo "Usage: $0 remove node3 [node4 ...]"; exit 1; }
        resolved=()
        for arg in "$@"; do
            resolved+=("$(resolve_node "$arg")")
        done
        echo "Removing ${#resolved[@]} worker(s)..."
        echo ""
        remove_nodes "${resolved[@]}"
        post_scale_summary
        ;;

    add)
        if [[ "${1:-}" == "all" ]]; then
            echo "Restoring all workers..."
            echo ""
            add_nodes "${WORKER_NODES[@]}"
        else
            [[ $# -lt 1 ]] && { echo "Usage: $0 add node3 [node4 ...] | add all"; exit 1; }
            resolved=()
            for arg in "$@"; do
                resolved+=("$(resolve_node "$arg")")
            done
            echo "Adding ${#resolved[@]} worker(s)..."
            echo ""
            add_nodes "${resolved[@]}"
        fi
        post_scale_summary
        ;;

    *)
        usage
        ;;
esac
