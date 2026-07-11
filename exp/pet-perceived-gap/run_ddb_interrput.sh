#!/bin/bash

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

DDB_DIR="$SCRIPT_DIR/../../ddb"
DDB_CONFIG_DIR="$DDB_DIR/configs"

if ! command -v ddb &> /dev/null
then
    echo "ddb could not be found"
    exit 1
fi

cleanup() {
    echo "cleaning up..."
    pkill -9 ddb
    pkill -9 faketime_pause
    rm "$PIPE"
    exit 0
}

trap cleanup INT 

set -e

# Create a named pipe
PIPE="/tmp/ddbpipe_$$"
mkfifo "$PIPE"

# Start pip
exec 3<>"$PIPE"

# Run ddb
ddb $DDB_CONFIG_DIR/dbg_auto_discovery.yaml <&3  > ./ddb.log 2>&1 &

echo "waiting ddb to start..."

# wait for ddb to start
sleep 8

echo "starting tester program..."
# Run tester program
cd $SCRIPT_DIR/..
make run_faketime_pause &

# wait for tester program to attach
sleep 1

echo "sending commands to ddb..."
echo "-exec-continue" > "$PIPE"

# pause-resume loop
sleep 1
while true; do
    # if ! pgrep -f faketime_pause >/dev/null; then
    #     break
    # fi
    echo "-exec-interrupt" > "$PIPE"
    sleep 0.01
    echo "-record-time-and-continue" > "$PIPE"
    sleep 0.01
    # usleep 20000
    # if [ $KILL -eq 1 ]; then
    #     break
    # fi
done

echo "finish loop"
cleanup
