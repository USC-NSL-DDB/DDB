#!/bin/bash
#
# Kill anything this experiment left behind: gdb (first, so it detaches instead of
# leaving the app ptrace-stopped), the app, ddb, and the fifo holder. Also removes
# the EMQX container DDB starts.
#
# run_experiment.sh does this on exit, including on Ctrl-C. Run it by hand after a
# kill -9 or a crash.

set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

echo "Stopping gdb / faketime_pause / ddb..."
reset_local

if docker info >/dev/null 2>&1 || sg docker -c 'docker info' >/dev/null 2>&1; then
  run_with_docker "docker rm -f emqx" >/dev/null 2>&1 && echo "Removed the emqx container."
fi

echo "Done."
