#!/usr/bin/env bash
# Shared configuration and helpers for the single-node call-depth deployment.

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
# Fixed application topology of the evaluated artifact; these are assertions,
# not knobs. The Kubernetes cluster may contain additional unused nodes.
readonly EXPECTED_DEPLOYMENTS=14
readonly EXPECTED_PROCESSES=14
readonly K3S_INSTALL_VERSION="v1.36.2+k3s1"
readonly KUBECTL_INSTALL_VERSION="v1.36.2"
readonly WEAVER_KUBE_INSTALL_VERSION="v0.23.0"
TARGET_NODE="${TARGET_NODE:-$(hostname)}"
DEFAULT_KUBECONFIG=/etc/rancher/k3s/k3s.yaml
KUBECONFIG="${KUBECONFIG:-$DEFAULT_KUBECONFIG}"
K3S_SERVICE="${K3S_SERVICE:-}"
K3S_BIN="${K3S_BIN:-}"
K3S_DATA_DIR="${K3S_DATA_DIR:-}"
K3S_EXEC_START=""
DDB_BIN="${DDB_BIN:-$DDB_SOURCE_DIR/target/release/ddb}"
DDB_CONFIG="$ARTIFACT_DIR/ddb/serviceweaver_config.yaml"
DDB_CONFIG_TEMPLATE="$ARTIFACT_DIR/ddb/serviceweaver_config.yaml.tmpl"
GATEWAY_MANIFEST="$ARTIFACT_DIR/ddb/ssh_gateway.yaml"
GATEWAY_POD_NAME="sw-socialnet-call-depth-ssh-gateway"
GATEWAY_SERVICE_NAME="sw-socialnet-call-depth-ssh-gateway"
SIDECAR_INJECTOR="$ARTIFACT_DIR/ddb/setup_debug_container.py"
SOCIALNET_CONFIG_TEMPLATE="$ARTIFACT_DIR/socialnet/config.yaml.tmpl"
SOCIALNET_WEAVER_TEMPLATE="$ARTIFACT_DIR/socialnet/weaver.toml.tmpl"
SOCIALNET_IMAGE="${SOCIALNET_IMAGE:-h21565897/socialnet-serviceweaver:12345}"
SOCIALNET_GO_IMAGE="${SOCIALNET_GO_IMAGE:-golang:1.21.1}"
SOCIALNET_BUILD_MODE="${SOCIALNET_BUILD_MODE:-docker}"
EXPECTED_SOCIALNET_COMMIT="${EXPECTED_SOCIALNET_COMMIT:-613f316ca060b94545e850324f91eef1ceb7639b}"
RESULTS_ROOT="${RESULTS_ROOT:-$ARTIFACT_DIR/results}"

export KUBECONFIG

die() {
  echo "Error: $*" >&2
  exit 1
}

note() {
  echo "==> $*"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required but was not found on PATH"
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
    if [[ ! -x "$service_bin" && -n "$K3S_BIN" && -x "$K3S_BIN" ]]; then
      service_bin="$K3S_BIN"
    fi
    if [[ ! -x "$service_bin" ]] && command -v k3s >/dev/null 2>&1; then
      service_bin="$(command -v k3s)"
    fi
    [[ -n "$service_bin" && -x "$service_bin" ]] || continue

    K3S_SERVICE="$candidate"
    K3S_BIN="$service_bin"
    K3S_EXEC_START="$exec_start"
    if [[ -z "$K3S_DATA_DIR" ]]; then
      detected_data_dir="$(sed -n 's/.*--data-dir=\([^ ;}]*\).*/\1/p' <<<"$exec_start")"
      K3S_DATA_DIR="${detected_data_dir:-/var/lib/rancher/k3s}"
    fi
    return 0
  done

  K3S_SERVICE=""
  K3S_BIN=""
  K3S_EXEC_START=""
  return 1
}

ensure_native_k3s() {
  detect_native_k3s \
    || die "no active native-k3s systemd service and executable were found.
  Run ./artifact.sh setup to install the pinned native-k3s release."
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

app_deployment_count() {
  k get deployments -l "$APP_LABEL_KEY" -o name 2>/dev/null | awk 'END { print NR + 0 }'
}

apilistener_service() {
  k get services -o name 2>/dev/null | awk '/apilistener/ && !seen++'
}

ensure_apilistener_nodeport() {
  local service
  service="$(apilistener_service)"
  [[ -n "$service" ]] || die "no apilistener service found; deploy the application first"
  k patch "$service" --type merge -p '{"spec":{"type":"NodePort"}}' >/dev/null
}

allow_target_node_workloads() {
  resolve_target_node
  kubectl --kubeconfig "$KUBECONFIG" taint node "$TARGET_NODE" \
    node-role.kubernetes.io/control-plane:NoSchedule- >/dev/null 2>&1 || true
  kubectl --kubeconfig "$KUBECONFIG" taint node "$TARGET_NODE" \
    node-role.kubernetes.io/master:NoSchedule- >/dev/null 2>&1 || true
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
      if [[ "$consecutive" -ge 3 ]]; then
        return 0
      fi
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
  local socialnet_commit
  socialnet_commit="$(git -C "$SOCIALNET_DIR" rev-parse HEAD 2>/dev/null || true)"
  [[ "$socialnet_commit" == "$EXPECTED_SOCIALNET_COMMIT" ]] \
    || die "SocialNet revision mismatch: expected $EXPECTED_SOCIALNET_COMMIT, found ${socialnet_commit:-unknown}
  The source-line breakpoints in this recipe are revision-specific."
}

validate_local_assets() {
  local path
  for path in "$DDB_CONFIG_TEMPLATE" "$GATEWAY_MANIFEST" "$SIDECAR_INJECTOR" \
    "$SOCIALNET_CONFIG_TEMPLATE" "$SOCIALNET_WEAVER_TEMPLATE" \
    "$ARTIFACT_DIR/prepare_socialnet.sh" "$ARTIFACT_DIR/setup_ddb.sh" \
    "$ARTIFACT_DIR/seed_data.sh" "$ARTIFACT_DIR/build_seeder.sh" \
    "$ARTIFACT_DIR/bootstrap_tools.sh" \
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
