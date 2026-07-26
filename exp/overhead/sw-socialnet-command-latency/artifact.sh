#!/usr/bin/env bash
# Front door for the full-cluster command-latency experiment.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  cat <<'EOF'
Usage: ./artifact.sh COMMAND [OPTIONS]

Commands:
  config     Print the resolved paths, topology, images, and overrides
  setup      Bootstrap k3s from workers.txt, deploy/seed app, and prepare DDB
  check      Validate the configured k3s nodes and application placement
  smoke      Run two DBTs on one thread across the attached cluster
  run        Run 30 DBTs for every thread across all cluster processes
  results    Print the latest aggregate table
EOF
}

command="${1:-}"
[[ -n "$command" ]] || { usage; exit 1; }
shift
case "$command" in
  config) exec "$HERE/show_config.sh" "$@" ;;
  setup) exec "$HERE/setup_experiment.sh" "$@" ;;
  check) exec "$HERE/check_cluster.sh" "$@" ;;
  smoke) exec "$HERE/run_command_latency.sh" --smoke "$@" ;;
  run) exec "$HERE/run_command_latency.sh" "$@" ;;
  results) exec "$HERE/show_results.sh" "$@" ;;
  -h|--help|help) usage ;;
  *) echo "Unknown command: $command" >&2; usage >&2; exit 1 ;;
esac
