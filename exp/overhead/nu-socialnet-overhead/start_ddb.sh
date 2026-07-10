#!/bin/bash
#
# Start DDB for a Nu run and make its broker reachable from every server node.
#
#   1. render ddb/nu_config.yaml from the template
#   2. launch ddb, holding its stdin open through a fifo so the REPL survives
#   3. wait for the managed EMQX broker + the service-discovery config
#   4. copy that config to the other caladan nodes (the Nu servers read it)
#
# run_benchmark.sh calls this. Run it directly only if you want an attached DDB
# session to drive by hand:  echo '-exec-continue' > logs/ddb_in

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

[[ -x "$DDB_BIN" ]] || die "ddb not built: $DDB_BIN
  cargo build --release --manifest-path $REPO_ROOT/ddb/Cargo.toml"
command -v docker >/dev/null || die "docker required (DDB starts EMQX in a container)"
docker info >/dev/null 2>&1 || die "cannot talk to docker as $(whoami). Run: newgrp docker"

mkdir -p "$LOG_DIR"
CALADAN_NIC="${CALADAN_NIC:-$(caladan_nic)}"; export CALADAN_NIC
SSH_PREFIX="${SSH_PREFIX:-$(ssh_prefix "$CALADAN_NIC")}"; export SSH_PREFIX

BROKER_IP="$(node_ip "$BACKEND_IDX")"   # DDB runs on the backend node
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

# Every Nu server reads this file locally; ship it to the other caladan nodes.
for i in $CTRL_IDX $CLIENT_IDX; do
  remote "$i" "sudo mkdir -p $(dirname "$DDB_SD_CONFIG") && sudo chown -R $(whoami) $(dirname "$DDB_SD_CONFIG")"
  scp -q "$DDB_SD_CONFIG" "$(node_ip "$i"):$DDB_SD_CONFIG" || die "could not copy sd config to node$i"
done
echo "  service-discovery config distributed"

echo "DDB is up. Send commands with:  echo '<gdb/mi cmd>' > $FIFO"
