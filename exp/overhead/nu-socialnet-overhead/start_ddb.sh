#!/bin/bash
#
# Start DDB on node0 for a distributed Nu run:
#   1. render ddb/nu_config.yaml (managed EMQX broker on node0)
#   2. launch ddb, holding its stdin open through a fifo so the REPL survives
#   3. wait for the broker + the service-discovery config
#   4. copy that config to every server node (each server reads it to find the
#      broker and report itself; DDB then ssh'es back to attach gdb)
#
# run_benchmark.sh calls this. Run it directly only to drive a session by hand:
#   echo '-exec-continue' > logs/ddb_in

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

[[ -x "$DDB_BIN" ]] || die "ddb not built: $DDB_BIN
  cargo build --release --manifest-path $REPO_ROOT/ddb/Cargo.toml"
command -v docker >/dev/null || die "docker required (DDB starts EMQX in a container)"
docker info >/dev/null 2>&1 || die "cannot talk to docker as $(whoami). Run: newgrp docker"

mkdir -p "$LOG_DIR"
detect_network

# Broker + DDB run here on node0; servers reach the broker over the 10.10.x
# ssh network (not caladan), so advertise node0's ssh IP.
BROKER_IP="$(node_ip "$INFRA_IDX")"
CONFIG_OUT="$EXP_DIR/ddb/nu_config.yaml"
FIFO="$LOG_DIR/ddb_in"
DDB_LOG="$LOG_DIR/ddb.log"

sed -e "s|@SSH_USER@|$(whoami)|g" \
    -e "s|@BROKER_IP@|$BROKER_IP|g" \
    -e "s|@LOG_DIR@|$HOME/ddb-tmp/logs|g" \
    -e "s|@BASE_DIR@|$HOME/ddb-tmp|g" \
    "$EXP_DIR/ddb/nu_config.yaml.tmpl" > "$CONFIG_OUT"
mkdir -p "$HOME/ddb-tmp/logs"
echo "Rendered $CONFIG_OUT (broker $BROKER_IP)"

# Fresh fifo + a writer that keeps it open, else ddb's REPL sees EOF and exits.
rm -f "$FIFO" "$DDB_LOG"; mkfifo "$FIFO"
sleep 86400 > "$FIFO" &
echo $! > "$LOG_DIR/ddb_holder.pid"

nohup "$DDB_BIN" "$CONFIG_OUT" --console-log < "$FIFO" > "$DDB_LOG" 2>&1 &
echo $! > "$LOG_DIR/ddb.pid"

echo "Waiting for the EMQX broker and service-discovery config..."
for _ in $(seq 1 45); do
  [[ -s "$DDB_SD_CONFIG" ]] && grep -q 'Successfully connected to broker' "$DDB_LOG" 2>/dev/null && break
  sleep 2
done
[[ -s "$DDB_SD_CONFIG" ]] || die "DDB never wrote $DDB_SD_CONFIG; see $DDB_LOG"
echo "  broker: $(head -1 "$DDB_SD_CONFIG")"

# Each Nu server reads this file locally; ship it to every server node.
for idx in "${SERVER_IDXS[@]}"; do
  remote "$idx" "sudo mkdir -p $(dirname "$DDB_SD_CONFIG") && sudo chown -R $(whoami) $(dirname "$DDB_SD_CONFIG")"
  scp -q "$DDB_SD_CONFIG" "$(node_ip "$idx"):$DDB_SD_CONFIG" || die "could not copy sd config to idx$idx"
done
echo "  service-discovery config distributed to ${#SERVER_IDXS[@]} servers"
