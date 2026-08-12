#!/usr/bin/env bash
# Front door for the single-node call-depth experiment.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  cat <<'EOF'
Usage: ./artifact.sh COMMAND [OPTIONS]

Commands:
  config     Optionally print the currently resolved configuration
  setup      Install missing cluster tools, deploy, and prepare all 14 processes
  check      Validate the one-node application topology and clean kernel state
  smoke      Run three DBTs at each of depths 2, 3, 4, 5, 6, and 10
  run        Run the full depth-2/3/4/5/6/10 evaluation
  results    Print the latest call-depth table
  restore    Remove call-depth debugger resources; keep SocialNet running
EOF
}

command="${1:-}"
[[ -n "$command" ]] || { usage; exit 1; }
shift
case "$command" in
  config) exec "$HERE/show_config.sh" "$@" ;;
  setup) exec "$HERE/setup_experiment.sh" "$@" ;;
  check) exec "$HERE/check_cluster.sh" "$@" ;;
  smoke) exec "$HERE/run_call_depth.sh" --smoke "$@" ;;
  run) exec "$HERE/run_call_depth.sh" "$@" ;;
  results) exec "$HERE/show_results.sh" "$@" ;;
  restore) exec "$HERE/restore_topology.sh" "$@" ;;
  -h|--help|help) usage ;;
  *) echo "Unknown command: $command" >&2; usage >&2; exit 1 ;;
esac
