#!/usr/bin/env bash
# Seed the running SocialNet deployment from the permitted application source.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

addr="${ADDR:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --addr) addr="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: $0 [--addr http://HOST:PORT]"
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

validate_source_inputs
bench_bin="$ARTIFACT_DIR/runtime/socialnet-seeder/init_social.out"
graph_file="$SOCIALNET_DIR/src/bench/social-graph/socfb-Reed98/socfb-Reed98.mtx"
[[ -r "$graph_file" ]] || die "SocialNet graph is missing: $graph_file"

bash "$ARTIFACT_DIR/build_seeder.sh"
[[ -x "$bench_bin" ]] || die "recipe-patched SocialNet seeder is missing: $bench_bin"

if [[ -z "$addr" ]]; then
  ensure_kubeconfig
  addr="$(detect_endpoint)"
fi

wait_for_application_endpoint "$addr"
note "Seeding the SocialNet graph at $addr"
"$bench_bin" -addr "$addr" -graph "$graph_file"
note "SocialNet graph seeded without request failures"
