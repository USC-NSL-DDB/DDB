#!/usr/bin/env bash
#
# setup_ddb.sh — Inject debug sidecar containers into all ServiceWeaver pods
#
# This MUST be run on the master node (node0) before attaching DDB to the cluster.
# It injects ephemeral containers with an SSH daemon into every app pod so DDB
# can tunnel through the ssh-gateway bastion and connect to each pod.
#
# Usage:
#   ./setup_ddb.sh            # inject sidecars + verify
#   ./setup_ddb.sh --check    # only check current sidecar status
#
set -euo pipefail

SETUP_SCRIPT="/local/tmp/setup_debug_container.py"
LABEL_SELECTOR="serviceweaver/app=server.out"

check_sidecars() {
    echo "=== Debug Sidecar Status ==="
    local total=0
    local running=0
    local not_running=0

    while IFS=$'\t' read -r pod_name status; do
        total=$((total + 1))
        if echo "$status" | grep -q '"running"'; then
            running=$((running + 1))
            echo "  ✓ $pod_name"
        elif [ -z "$status" ]; then
            not_running=$((not_running + 1))
            echo "  ✗ $pod_name (no sidecar)"
        else
            not_running=$((not_running + 1))
            echo "  ⏳ $pod_name ($status)"
        fi
    done < <(kubectl get pods -l "$LABEL_SELECTOR" -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{range .status.ephemeralContainerStatuses[*]}{.state}{end}{"\n"}{end}')

    echo ""
    echo "Total: $total pods | Running: $running | Not ready: $not_running"
    return $not_running
}

# --check mode: just report status
if [[ "${1:-}" == "--check" ]]; then
    check_sidecars
    exit $?
fi

# Verify prerequisites
if [[ ! -f "$SETUP_SCRIPT" ]]; then
    echo "ERROR: $SETUP_SCRIPT not found"
    exit 1
fi

if ! python3 -c "import kubernetes" 2>/dev/null; then
    echo "ERROR: python3 kubernetes package not installed"
    echo "  pip3 install kubernetes"
    exit 1
fi

# Check how many pods need sidecars
pod_count=$(kubectl get pods -l "$LABEL_SELECTOR" --no-headers 2>/dev/null | wc -l)
if [[ "$pod_count" -eq 0 ]]; then
    echo "ERROR: No pods found with label $LABEL_SELECTOR"
    echo "  Is the ServiceWeaver app deployed?"
    exit 1
fi
echo "Found $pod_count app pods"

# Check if sidecars already injected
if check_sidecars 2>/dev/null; then
    echo ""
    echo "All sidecars already running. Nothing to do."
    echo "  Use --check to re-verify at any time."
    exit 0
fi

echo ""
echo "Injecting debug sidecar containers..."
python3 "$SETUP_SCRIPT"

# Wait for all sidecars to start (up to 60s)
echo ""
echo "Waiting for sidecars to start..."
for i in $(seq 1 12); do
    sleep 5
    if check_sidecars 2>/dev/null; then
        echo ""
        echo "✓ All debug sidecars running. DDB is ready to connect."
        echo ""
        echo "Next: run DDB with config at /local/tmp/serviceweaver_config.yaml"
        exit 0
    fi
    echo "  ...waiting ($((i * 5))s)"
done

echo ""
echo "WARNING: Some sidecars still not running after 60s."
echo "  Run './setup_ddb.sh --check' to re-check."
exit 1
