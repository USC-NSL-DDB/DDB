#!/bin/bash
# WARNING: This script executes "ddb_runapp" with sudo.
# This typically requires passwordless sudo configuration for "ddb_runapp" or the current user,
# which has significant security implications.

PREFIX=$HOME/.local/bin

echo "[$(date)] DDB runapp sudo wrapper called with args: $@" >> /tmp/ddb-runapp-sudo-wrapper.log

DDB_RUNAPP_PATH="$PREFIX/ddb_runapp" # Or /path/to/your/ddb_runapp

exec sudo ${DDB_RUNAPP_PATH} "$@"
