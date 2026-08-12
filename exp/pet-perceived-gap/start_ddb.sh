#!/bin/bash
#
# Start DDB for the perceived-time-gap run:
#   1. render ddb/pet_config.yaml
#   2. launch ddb, holding its stdin open through a fifo so the MI REPL survives
#   3. wait for the broker and for the service-discovery config to appear -- the
#      app reads that file at startup to find the broker and report itself, so it
#      must exist before the app starts
#
# run_experiment.sh calls this. Run it directly only to drive a session by hand:
#   echo '-exec-interrupt'             > logs/ddb_in
#   echo '-record-time-and-continue'   > logs/ddb_in

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/common.sh"

[[ -x "$DDB_BIN" ]] || die "ddb not built: $DDB_BIN (run ./build_all.sh)"

mkdir -p "$LOG_DIR"

CONFIG_OUT="$EXP_DIR/ddb/pet_config.yaml"
FIFO="$LOG_DIR/ddb_in"
DDB_LOG="$LOG_DIR/ddb.log"

sed -e "s|@SSH_USER@|$(whoami)|g" \
    -e "s|@BROKER_IP@|$BROKER_IP|g" \
    -e "s|@APP_IP@|$APP_IP|g" \
    -e "s|@LOG_DIR@|$HOME/ddb-tmp/logs|g" \
    -e "s|@BASE_DIR@|$HOME/ddb-tmp|g" \
    "$EXP_DIR/ddb/pet_config.yaml.tmpl" > "$CONFIG_OUT"
mkdir -p "$HOME/ddb-tmp/logs"
echo "Rendered $CONFIG_OUT (broker $BROKER_IP, app $APP_IP)"

# DDB rewrites the service-discovery config on startup. Remove the old one so the
# wait below cannot pass on a leftover from a previous run.
sudo rm -f "$DDB_SD_CONFIG"

# Fresh fifo, plus a writer that holds it open -- otherwise ddb's MI REPL sees
# EOF on stdin as soon as the first `echo >` closes, and exits.
rm -f "$FIFO" "$DDB_LOG"; mkfifo "$FIFO"
sleep 86400 > "$FIFO" &
echo $! > "$LOG_DIR/ddb_holder.pid"

# DDB shells out to plain `docker` to run the EMQX broker, so it needs the docker
# group in its current credentials (see docker_launch_prefix in common.sh).
run_with_docker "nohup '$DDB_BIN' '$CONFIG_OUT' --console-log < '$FIFO' > '$DDB_LOG' 2>&1 & echo \$! > '$LOG_DIR/ddb.pid'"

echo "Waiting for the EMQX broker and the service-discovery config..."
for _ in $(seq 1 60); do
  if [[ -s "$DDB_SD_CONFIG" ]] && grep -q 'Successfully connected to broker' "$DDB_LOG" 2>/dev/null; then
    break
  fi
  # If ddb died on a config error, say so now instead of after a 2-minute wait.
  if ! kill -0 "$(cat "$LOG_DIR/ddb.pid" 2>/dev/null)" 2>/dev/null; then
    die "ddb exited during startup. Last lines of $DDB_LOG:
$(tail -20 "$DDB_LOG" 2>/dev/null)"
  fi
  sleep 2
done
[[ -s "$DDB_SD_CONFIG" ]] || die "DDB never wrote $DDB_SD_CONFIG; see $DDB_LOG"

# The app runs as us and reads this file; DDB may have written it as root.
sudo chown -R "$(whoami)" "$(dirname "$DDB_SD_CONFIG")" 2>/dev/null || true
echo "  broker: $(head -1 "$DDB_SD_CONFIG")"
echo "DDB is up (pid $(cat "$LOG_DIR/ddb.pid"))."
