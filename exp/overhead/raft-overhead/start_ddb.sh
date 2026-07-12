#!/bin/bash
#
# Start DDB on node0 for the distributed raft run:
#   1. render ddb/raft_config.yaml (managed EMQX broker on node0)
#   2. launch ddb, holding its stdin open through a fifo so the REPL survives
#   3. wait for the broker + the service-discovery config
#   4. copy that config to every raft node -- each raft_node reads it at startup
#      to find the broker and report itself; DDB then ssh'es back and attaches gdb
#
# run_benchmark.sh --mode ddb calls this. Run it directly only to drive a session
# by hand:
#   echo '-exec-continue' > logs/ddb_in

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

[[ -x "$DDB_BIN" ]] || die "ddb not built: $DDB_BIN
  cargo build --release --manifest-path $REPO_ROOT/ddb/Cargo.toml"
command -v docker >/dev/null || die "docker required (DDB starts EMQX in a container)"
docker info >/dev/null 2>&1 || die "cannot talk to docker as $(whoami). Run: newgrp docker"

mkdir -p "$LOG_DIR"

CONFIG_OUT="$EXP_DIR/ddb/raft_config.yaml"
FIFO="$LOG_DIR/ddb_in"
DDB_LOG="$LOG_DIR/ddb.log"

sed -e "s|@SSH_USER@|$(whoami)|g" \
    -e "s|@BROKER_IP@|$HEAD_IP|g" \
    -e "s|@LOG_DIR@|$HOME/ddb-tmp/logs|g" \
    -e "s|@BASE_DIR@|$HOME/ddb-tmp|g" \
    "$EXP_DIR/ddb/raft_config.yaml.tmpl" > "$CONFIG_OUT"
mkdir -p "$HOME/ddb-tmp/logs"
echo "Rendered $CONFIG_OUT (broker $HEAD_IP)"

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

# Each raft_node reads this file locally at startup; ship it to every server.
for ip in "${SERVER_IPS[@]}"; do
  remote "$ip" "sudo mkdir -p $(dirname "$DDB_SD_CONFIG") && sudo chown -R $(whoami) $(dirname "$DDB_SD_CONFIG")"
  scp -q "$DDB_SD_CONFIG" "$ip:$DDB_SD_CONFIG" || die "could not copy sd config to $ip"
done
echo "  service-discovery config distributed to $NUM_SERVERS raft nodes"
