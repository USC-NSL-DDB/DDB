#!/usr/bin/env bash
# Seed SocialNet directly from the accepted ServiceWeaver application source tree.
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"
require_command curl

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

bench="$ARTIFACT_DIR/runtime/socialnet-seeder/init_social.out"
graph="$SOCIALNET_DIR/src/bench/social-graph/socfb-Reed98/socfb-Reed98.mtx"
[[ -r "$graph" ]] || die "ServiceWeaver graph input not found: $graph"

bash "$ARTIFACT_DIR/build_seeder.sh"
[[ -x "$bench" ]] || die "recipe-patched SocialNet seeder is missing: $bench"

if [[ -z "$addr" ]]; then
  ensure_kubeconfig
  addr="$(detect_endpoint)"
fi
wait_for_application_endpoint "$addr"
note "Seeding SocialNet at $addr"
"$bench" -addr "$addr" -graph "$graph"
note "SocialNet seeding completed"
