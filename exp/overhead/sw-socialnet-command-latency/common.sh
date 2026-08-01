#!/usr/bin/env bash
# Shared configuration and helpers for the DDB latency artifact recipe.

ARTIFACT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="$HOME/.local/bin:/usr/local/go/bin:$HOME/go/bin:$HOME/.cargo/bin:$PATH"

# A private override file keeps machine-specific values out of the recipe.
if [[ -f "$ARTIFACT_DIR/artifact.env" ]]; then
  # shellcheck disable=SC1091
  source "$ARTIFACT_DIR/artifact.env"
fi

if [[ -n "${KUBECONFIG:-}" ]]; then
  KUBECONFIG_EXPLICIT=1
else
  KUBECONFIG_EXPLICIT=0
fi

# Only the implementation under test and the application source are external
# inputs. Every experiment config and helper lives under ARTIFACT_DIR.
discover_ddb_repo_root() {
  local candidate
  for candidate in \
    "$(git -C "$ARTIFACT_DIR" rev-parse --show-toplevel 2>/dev/null || true)" \
    "$ARTIFACT_DIR/../../.." \
    "$HOME/DDB"; do
    [[ -n "$candidate" ]] || continue
    candidate="$(cd "$candidate" 2>/dev/null && pwd || true)"
    if [[ -f "$candidate/ddb/Cargo.toml" && -d "$candidate/fwks/socialnetwork" ]]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

DDB_REPO_ROOT="${DDB_REPO_ROOT:-$(discover_ddb_repo_root || true)}"
DDB_SOURCE_DIR="${DDB_SOURCE_DIR:-${DDB_REPO_ROOT:+$DDB_REPO_ROOT/ddb}}"
SOCIALNET_DIR="${SOCIALNET_DIR:-${DDB_REPO_ROOT:+$DDB_REPO_ROOT/fwks/socialnetwork}}"

NAMESPACE="${NAMESPACE:-default}"
APP_LABEL_KEY="${APP_LABEL_KEY:-serviceweaver/app}"
DEBUGGER_CONTAINER_PREFIX="${DEBUGGER_CONTAINER_PREFIX:-ssh-debugger-}"
# Worker count and process replication are independent configuration axes.
readonly EXPECTED_DEPLOYMENTS=14
SOCIALNET_REPLICAS="${SOCIALNET_REPLICAS:-1}"
COMMAND_WORKERS="${COMMAND_WORKERS:-20}"
EXPECTED_PROCESSES=0
EXPECTED_WORKERS=0
EXPECTED_CLUSTER_NODES=1
readonly K3S_INSTALL_VERSION="v1.36.2+k3s1"
readonly KUBECTL_INSTALL_VERSION="v1.36.2"
readonly WEAVER_KUBE_INSTALL_VERSION="v0.23.0"
CONTROLLER_IP="${CONTROLLER_IP:-}"
WORKERS_FILE="${WORKERS_FILE:-$ARTIFACT_DIR/workers.txt}"
SSH_IDENTITY_FILE="${SSH_IDENTITY_FILE:-}"
SSH_CONNECT_TIMEOUT="${SSH_CONNECT_TIMEOUT:-10}"
ALLOW_ACTIVE_FIREWALL="${ALLOW_ACTIVE_FIREWALL:-0}"
TARGET_NODE="${TARGET_NODE:-$(hostname)}"
DEFAULT_KUBECONFIG=/etc/rancher/k3s/k3s.yaml
KUBECONFIG="${KUBECONFIG:-$DEFAULT_KUBECONFIG}"
K3S_SERVICE="${K3S_SERVICE:-}"
K3S_BIN="${K3S_BIN:-}"
K3S_DATA_DIR="${K3S_DATA_DIR:-/var/lib/rancher/k3s}"
K3S_EXEC_START=""
DDB_BIN="${DDB_BIN:-$DDB_SOURCE_DIR/target/release/ddb}"
DDB_CONFIG="$ARTIFACT_DIR/ddb/serviceweaver_config.yaml"
DDB_CONFIG_TEMPLATE="$ARTIFACT_DIR/ddb/serviceweaver_config.yaml.tmpl"
GATEWAY_MANIFEST="$ARTIFACT_DIR/ddb/ssh_gateway.yaml"
GATEWAY_NAME="ddb-command-latency-gateway"
SIDECAR_INJECTOR="$ARTIFACT_DIR/ddb/setup_debug_container.py"
SOCIALNET_CONFIG_TEMPLATE="$ARTIFACT_DIR/socialnet/config.yaml.tmpl"
SOCIALNET_APP_TEMPLATE="$ARTIFACT_DIR/socialnet/weaver.toml.tmpl"
SOCIALNET_IMAGE="${SOCIALNET_IMAGE:-h21565897/socialnet-serviceweaver:latest}"
SOCIALNET_GO_IMAGE="${SOCIALNET_GO_IMAGE:-golang:1.21.1}"
RESULTS_ROOT="${RESULTS_ROOT:-$ARTIFACT_DIR/results}"

export KUBECONFIG

die() {
  echo "Error: $*" >&2
  exit 1
}

[[ "$SOCIALNET_REPLICAS" =~ ^[1-9][0-9]*$ ]] \
  || die "SOCIALNET_REPLICAS must be a positive integer, got '$SOCIALNET_REPLICAS'"
[[ "$COMMAND_WORKERS" =~ ^[1-9][0-9]*$ ]] \
  || die "COMMAND_WORKERS must be a positive integer, got '$COMMAND_WORKERS'"
EXPECTED_PROCESSES=$((EXPECTED_DEPLOYMENTS * SOCIALNET_REPLICAS))
readonly SOCIALNET_REPLICAS COMMAND_WORKERS EXPECTED_PROCESSES

note() {
  echo "==> $*"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required but was not found on PATH"
}

load_worker_targets() {
  [[ -r "$WORKERS_FILE" ]] || die "worker inventory not found: $WORKERS_FILE
  Copy workers.txt.example to workers.txt and list one or more SSH targets."

  WORKER_TARGETS=()
  local line unique_workers
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%%#*}"
    line="$(awk '{$1=$1; print}' <<<"$line")"
    if [[ -n "$line" ]]; then
      [[ "$line" != *[[:space:]]* ]] \
        || die "each workers.txt entry must be one SSH target without options: '$line'"
      WORKER_TARGETS+=("$line")
    fi
  done < "$WORKERS_FILE"

  [[ "${#WORKER_TARGETS[@]}" -gt 0 ]] \
    || die "command latency requires at least one worker SSH target in $WORKERS_FILE"
  unique_workers="$(printf '%s\n' "${WORKER_TARGETS[@]}" | sort -u | wc -l)"
  [[ "$unique_workers" -eq "${#WORKER_TARGETS[@]}" ]] \
    || die "$WORKERS_FILE contains duplicate worker SSH targets"

  EXPECTED_WORKERS="${#WORKER_TARGETS[@]}"
  EXPECTED_CLUSTER_NODES=$((EXPECTED_WORKERS + 1))
}

validate_cluster_inputs() {
  [[ -n "$CONTROLLER_IP" ]] || die "CONTROLLER_IP is not configured.
  Copy artifact.env.example to artifact.env and set the controller's private cluster IP."
  [[ "$CONTROLLER_IP" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "CONTROLLER_IP must be an IPv4 address, got '$CONTROLLER_IP'"
  ip -o -4 addr show | awk '{print $4}' | cut -d/ -f1 | grep -qxF "$CONTROLLER_IP" \
    || die "CONTROLLER_IP $CONTROLLER_IP is not assigned to this controller"
  if [[ -n "$SSH_IDENTITY_FILE" ]]; then
    [[ -r "$SSH_IDENTITY_FILE" ]] \
      || die "SSH_IDENTITY_FILE is not readable: $SSH_IDENTITY_FILE"
  fi
  [[ "$SSH_CONNECT_TIMEOUT" =~ ^[1-9][0-9]*$ ]] \
    || die "SSH_CONNECT_TIMEOUT must be a positive integer"
  load_worker_targets
}

worker_ssh() {
  local target="$1"
  shift
  local -a args=(
    ssh
    -o BatchMode=yes
    -o "ConnectTimeout=$SSH_CONNECT_TIMEOUT"
    -o StrictHostKeyChecking=accept-new
  )
  [[ -z "$SSH_IDENTITY_FILE" ]] || args+=(-i "$SSH_IDENTITY_FILE")
  "${args[@]}" "$target" "$@"
}

detect_native_k3s() {
  local candidate exec_start service_bin detected_data_dir
  local -a candidates=()
  [[ -z "$K3S_SERVICE" ]] || candidates+=("$K3S_SERVICE")
  candidates+=(k3s.service)
  while IFS= read -r candidate; do
    [[ -z "$candidate" ]] || candidates+=("$candidate")
  done < <(systemctl list-units --type=service --state=running \
    --no-legend --plain 2>/dev/null \
    | awk '$1 ~ /^k3s.*\.service$/ { print $1 }')

  for candidate in "${candidates[@]}"; do
    [[ "$candidate" != *agent* ]] || continue
    systemctl is-active --quiet "$candidate" || continue
    exec_start="$(systemctl show -p ExecStart --value "$candidate" 2>/dev/null || true)"
    [[ "$exec_start" == *" server "* ]] || continue
    service_bin="$(sed -n 's/.*path=\([^ ;}]*\).*/\1/p' <<<"$exec_start")"
    if [[ ! -x "$service_bin" ]] && command -v k3s >/dev/null 2>&1; then
      service_bin="$(command -v k3s)"
    fi
    [[ -n "$service_bin" && -x "$service_bin" ]] || continue

    K3S_SERVICE="$candidate"
    K3S_BIN="$service_bin"
    K3S_EXEC_START="$exec_start"
    detected_data_dir="$(sed -n 's/.*--data-dir=\([^ ;}]*\).*/\1/p' <<<"$exec_start")"
    K3S_DATA_DIR="${detected_data_dir:-$K3S_DATA_DIR}"
    return 0
  done

  K3S_SERVICE=""
  K3S_BIN=""
  K3S_EXEC_START=""
  return 1
}

ensure_native_k3s() {
  detect_native_k3s \
    || die "no active native-k3s server was found on the controller.
  Run ./artifact.sh setup to install and configure the pinned cluster."
}

ensure_kubeconfig() {
  [[ -r "$KUBECONFIG" ]] || die "kubeconfig is not readable: $KUBECONFIG
  Set KUBECONFIG in $ARTIFACT_DIR/artifact.env."
}

k() {
  kubectl --kubeconfig "$KUBECONFIG" -n "$NAMESPACE" "$@"
}

resolve_target_node() {
  if kubectl --kubeconfig "$KUBECONFIG" get node "$TARGET_NODE" >/dev/null 2>&1; then
    return 0
  fi

  local detected
  detected="$(kubectl --kubeconfig "$KUBECONFIG" get nodes \
    -l node-role.kubernetes.io/control-plane \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
  [[ -n "$detected" ]] || die "Kubernetes node '$TARGET_NODE' does not exist and no control-plane node was detected.
  Set TARGET_NODE in $ARTIFACT_DIR/artifact.env."
  TARGET_NODE="$detected"
}

app_label_value() {
  k get deployments -l "$APP_LABEL_KEY" -o json 2>/dev/null \
    | python3 -c '
import json, sys

key = sys.argv[1]
seen = set()
for deployment in json.load(sys.stdin).get("items", []):
    value = deployment.get("metadata", {}).get("labels", {}).get(key)
    if value and value not in seen:
        print(value)
        seen.add(value)
' "$APP_LABEL_KEY"
}

app_selector() {
  local value
  value="$(app_label_value)"
  [[ -n "$value" ]] || die "no ServiceWeaver deployments found with label $APP_LABEL_KEY"
  echo "$APP_LABEL_KEY=$value"
}

app_deployments() {
  k get deployments -l "$(app_selector)" -o name
}

wait_for_exact_app_pods() {
  local selector="${1:-$(app_selector)}"
  local attempts="${2:-60}"
  local total ready terminating
  for _ in $(seq 1 "$attempts"); do
    read -r total ready terminating <<<"$(k get pods -l "$selector" -o json | python3 -c '
import json, sys
items = json.load(sys.stdin).get("items", [])
ready = sum(
    not pod.get("metadata", {}).get("deletionTimestamp")
    and pod.get("status", {}).get("phase") == "Running"
    and all(c.get("ready") for c in pod.get("status", {}).get("containerStatuses", []))
    for pod in items
)
terminating = sum(bool(pod.get("metadata", {}).get("deletionTimestamp")) for pod in items)
print(len(items), ready, terminating)
')"
    if [[ "$total" -eq "$EXPECTED_PROCESSES" && "$ready" -eq "$EXPECTED_PROCESSES" \
      && "$terminating" -eq 0 ]]; then
      return 0
    fi
    sleep 2
  done
  die "application pod set did not settle: total=$total ready=$ready terminating=$terminating"
}

apilistener_service() {
  k get services -o name 2>/dev/null | awk '/apilistener/ && !seen++'
}

detect_endpoint() {
  if [[ -n "${ADDR:-}" ]]; then
    echo "$ADDR"
    return 0
  fi

  resolve_target_node
  local service node_port node_ip
  service="$(apilistener_service)"
  [[ -n "$service" ]] || die "no apilistener service found; deploy the application first"
  node_port="$(k get "$service" -o jsonpath='{.spec.ports[0].nodePort}')"
  [[ -n "$node_port" ]] || die "$service is not exposed as a NodePort"
  node_ip="$(kubectl --kubeconfig "$KUBECONFIG" get node "$TARGET_NODE" \
    -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')"
  [[ -n "$node_ip" ]] || die "could not determine the InternalIP of $TARGET_NODE"
  echo "http://$node_ip:$node_port"
}

wait_for_application_endpoint() {
  local endpoint="${1:-$(detect_endpoint)}"
  local timeout_seconds="${2:-300}"
  local deadline=$((SECONDS + timeout_seconds))
  local http_code consecutive=0

  note "Waiting for the SocialNet API at $endpoint"
  while (( SECONDS < deadline )); do
    http_code="$(curl -sS -o /dev/null --connect-timeout 2 --max-time 5 \
      -w '%{http_code}' "$endpoint/" 2>/dev/null || true)"
    if [[ "$http_code" == "404" ]]; then
      consecutive=$((consecutive + 1))
      [[ "$consecutive" -ge 3 ]] && return 0
    else
      consecutive=0
    fi
    sleep 2
  done
  die "SocialNet API did not become stable at $endpoint within ${timeout_seconds}s (last HTTP status: ${http_code:-000})"
}

ensure_no_ddb() {
  if pgrep -x ddb >/dev/null 2>&1; then
    die "a DDB process is already running. Exit it before starting an artifact run."
  fi
  if curl -fsS --max-time 1 http://127.0.0.1:5000/status >/dev/null 2>&1; then
    die "DDB API port 5000 is already active. Stop the existing debugger first."
  fi
}

validate_runtime_config() {
  local extension_line runtime_line
  [[ -r "$DDB_CONFIG" ]] || die "DDB config not found: $DDB_CONFIG
  Run ./artifact.sh setup."
  extension_line="$(grep -nF 'source /workspace/extension.py' "$DDB_CONFIG" \
    | head -1 | cut -d: -f1 || true)"
  [[ -n "$extension_line" ]] \
    || die "$DDB_CONFIG does not load extension.py; run ./artifact.sh setup"
  runtime_line="$(grep -nF 'source /workspace/runtime-serviceweaver.py' "$DDB_CONFIG" \
    | head -1 | cut -d: -f1 || true)"
  [[ -n "$runtime_line" ]] \
    || die "$DDB_CONFIG does not load runtime-serviceweaver.py; run ./artifact.sh setup"
  [[ "$extension_line" -lt "$runtime_line" ]] \
    || die "$DDB_CONFIG must load extension.py before runtime-serviceweaver.py"
}

validate_ddb_binary() {
  [[ -x "$DDB_BIN" ]] || die "Rust DDB binary not found: $DDB_BIN
  Run ./artifact.sh setup to build it."
}

validate_source_inputs() {
  [[ -n "$DDB_SOURCE_DIR" ]] || die "could not locate the DDB Rust source.
  Set DDB_SOURCE_DIR in $ARTIFACT_DIR/artifact.env."
  [[ -f "$DDB_SOURCE_DIR/Cargo.toml" ]] || die "DDB Rust source not found: $DDB_SOURCE_DIR
  Set DDB_SOURCE_DIR in $ARTIFACT_DIR/artifact.env."
  [[ -n "$SOCIALNET_DIR" ]] || die "could not locate the ServiceWeaver SocialNet source.
  Set SOCIALNET_DIR in $ARTIFACT_DIR/artifact.env."
  [[ -d "$SOCIALNET_DIR" ]] || die "ServiceWeaver SocialNet source not found: $SOCIALNET_DIR
  Set SOCIALNET_DIR in $ARTIFACT_DIR/artifact.env."
}

validate_local_assets() {
  local path
  for path in "$DDB_CONFIG_TEMPLATE" "$GATEWAY_MANIFEST" "$SIDECAR_INJECTOR" \
    "$ARTIFACT_DIR/setup_ddb.sh" "$ARTIFACT_DIR/seed_data.sh" \
    "$ARTIFACT_DIR/build_seeder.sh" "$ARTIFACT_DIR/prepare_socialnet.sh" \
    "$ARTIFACT_DIR/bootstrap_cluster.sh" \
    "$SOCIALNET_CONFIG_TEMPLATE" "$SOCIALNET_APP_TEMPLATE" \
    "$ARTIFACT_DIR/socialnet/init-social-graph-rand.patch"; do
    [[ -r "$path" ]] || die "recipe-owned asset is missing: $path"
  done
}

timestamp() {
  date -u +%Y%m%d_%H%M%S
}

show_csv() {
  local path="$1"
  [[ -r "$path" ]] || return 0
  echo ""
  echo "--- $path"
  if command -v column >/dev/null 2>&1; then
    column -s, -t < "$path"
  else
    cat "$path"
  fi
}
