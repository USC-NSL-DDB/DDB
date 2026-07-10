#!/usr/bin/env bash
#
# setup_ddb.sh — Prepare the cluster for DDB, end to end.
#
#   1. Deploy the ssh-gateway bastion pod + NodePort service
#   2. Inject ephemeral SSH debug sidecars into every ServiceWeaver app pod
#   3. Render a ready-to-use DDB config with the discovered service name,
#      kubeconfig path, and gateway ClusterIP filled in
#
# Run on the master node (node0), after the app is deployed. Pod restarts drop
# ephemeral containers, so re-run this after ./redeploy_app.sh or
# ./scale_cluster.sh. Every step is idempotent.
#
# Usage:
#   ./setup_ddb.sh            # gateway + sidecars + config
#   ./setup_ddb.sh --check    # report sidecar status only
#
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

DDB_DIR="$EXP_DIR/ddb"
GATEWAY_YAML="$DDB_DIR/ssh_gateway.yaml"
INJECT_SCRIPT="$DDB_DIR/setup_debug_container.py"
CONFIG_TMPL="$DDB_DIR/serviceweaver_config.yaml.tmpl"
CONFIG_OUT="$DDB_DIR/serviceweaver_config.yaml"

ensure_kubeconfig

# ─── Sidecar status ──────────────────────────────────────────────────────────

check_sidecars() {
    local label="$1"
    echo "=== Debug Sidecar Status ==="
    local total=0 running=0 not_running=0

    while IFS=$'\t' read -r pod_name status; do
        [[ -z "$pod_name" ]] && continue
        total=$((total + 1))
        if echo "$status" | grep -q '"running"'; then
            running=$((running + 1))
            echo "  ✓ $pod_name"
        elif [[ -z "$status" ]]; then
            not_running=$((not_running + 1))
            echo "  ✗ $pod_name (no sidecar)"
        else
            not_running=$((not_running + 1))
            echo "  ⏳ $pod_name ($status)"
        fi
    done < <(kubectl get pods -n "$NAMESPACE" -l "$APP_LABEL_KEY=$label" \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{range .status.ephemeralContainerStatuses[*]}{.state}{end}{"\n"}{end}')

    echo ""
    echo "Total: $total pods | Running: $running | Not ready: $not_running"
    return "$not_running"
}

# ─── Discover the app label ──────────────────────────────────────────────────

APP_LABEL="$(app_label_value)"
[[ -n "$APP_LABEL" ]] || die "no deployments carry the $APP_LABEL_KEY label. Is the app deployed? (./deploy_app.sh)"
echo "ServiceWeaver app label: $APP_LABEL_KEY=$APP_LABEL"

if [[ "${1:-}" == "--check" ]]; then
    check_sidecars "$APP_LABEL"
    exit $?
fi

pod_count=$(kubectl get pods -n "$NAMESPACE" -l "$APP_LABEL_KEY=$APP_LABEL" --no-headers 2>/dev/null | wc -l)
[[ "$pod_count" -gt 0 ]] || die "no running pods with label $APP_LABEL_KEY=$APP_LABEL"
echo "Found $pod_count app pods"

# ─── Step 1: ssh-gateway ─────────────────────────────────────────────────────

echo ""
echo "=== Step 1: ssh-gateway bastion ==="
if kubectl get svc ssh-gateway -n "$NAMESPACE" >/dev/null 2>&1; then
    echo "ssh-gateway already deployed."
else
    kubectl apply -f "$GATEWAY_YAML"
    echo "Waiting for ssh-gateway pod to be Ready..."
    kubectl wait --for=condition=Ready pod/ssh-gateway -n "$NAMESPACE" --timeout=180s
fi
GATEWAY_IP="$(kubectl get svc ssh-gateway -n "$NAMESPACE" -o jsonpath='{.spec.clusterIP}')"
[[ -n "$GATEWAY_IP" ]] || die "could not read ssh-gateway ClusterIP"
echo "ssh-gateway ClusterIP: $GATEWAY_IP"

# ─── Step 2: debug sidecars ──────────────────────────────────────────────────

echo ""
echo "=== Step 2: Debug sidecars ==="
python3 -c "import kubernetes" 2>/dev/null || die "python3 kubernetes package not installed.
  pip3 install kubernetes"

if check_sidecars "$APP_LABEL" 2>/dev/null; then
    echo "All sidecars already running."
else
    echo ""
    echo "Injecting debug sidecar containers..."
    python3 "$INJECT_SCRIPT" --kubeconfig "$KUBECONFIG" --label "$APP_LABEL" --namespace "$NAMESPACE"

    echo ""
    echo "Waiting for sidecars to start..."
    ready=0
    for i in $(seq 1 12); do
        sleep 5
        if check_sidecars "$APP_LABEL" 2>/dev/null; then
            ready=1
            break
        fi
        echo "  ...waiting ($((i * 5))s)"
    done
    if [[ "$ready" -eq 0 ]]; then
        echo ""
        echo "WARNING: some sidecars still not running after 60s." >&2
        echo "  Re-check with: ./setup_ddb.sh --check" >&2
        exit 1
    fi
fi

# ─── Step 3: render DDB config ───────────────────────────────────────────────

echo ""
echo "=== Step 3: DDB config ==="
LOG_DIR="${DDB_LOG_DIR:-$HOME/ddb-tmp/logs}"
BASE_DIR="${DDB_BASE_DIR:-$HOME/ddb-tmp}"
mkdir -p "$LOG_DIR" "$BASE_DIR"

sed -e "s|@SERVICE_NAME@|$APP_LABEL|g" \
    -e "s|@KUBECONFIG_PATH@|$KUBECONFIG|g" \
    -e "s|@GATEWAY_IP@|$GATEWAY_IP|g" \
    -e "s|@LOG_DIR@|$LOG_DIR|g" \
    -e "s|@BASE_DIR@|$BASE_DIR|g" \
    "$CONFIG_TMPL" > "$CONFIG_OUT"
echo "Wrote $CONFIG_OUT"

echo ""
echo "=== DDB is ready ==="
echo "  service_name:        $APP_LABEL"
echo "  kubectl_config_path: $KUBECONFIG"
echo "  jump_clinet_host:    $GATEWAY_IP"
echo ""
echo "Launch the debugger with (the config path is positional, not --config):"
echo "  ddb $CONFIG_OUT"
echo ""
echo "If 'ddb' is not on PATH, build it first from the repo root:"
echo "  make setup && cargo build --release --manifest-path ddb/Cargo.toml"
