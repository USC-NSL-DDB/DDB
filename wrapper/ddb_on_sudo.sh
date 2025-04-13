#!/bin/bash
# WARNING: This script executes DDB with sudo.
# This typically requires passwordless sudo configuration for DDB or the current user,
# which has significant security implications.

PREFIX=$HOME/.cargo/bin

echo "[$(date)] DDB sudo wrapper called with args: $@" >> /tmp/ddb-sudo-wrapper.log

DDB_PATH="$PREFIX/ddb" # Or /path/to/your/ddb

exec sudo ${DDB_PATH} "$@"
