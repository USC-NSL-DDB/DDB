#!/bin/bash
# WARNING: This script executes DDB with sudo.
# This typically requires passwordless sudo configuration for DDB or the current user,
# which has significant security implications.

VERSION="0.1.0"

# Handle version flag
if [ "$1" = "--version" ] || [ "$1" = "-v" ]; then
    echo "ddb_on_sudo v${VERSION}"
    exit 0
fi

PREFIX=$HOME/.cargo/bin

echo "[$(date)] DDB sudo wrapper called with args: $@" >> /tmp/ddb-sudo-wrapper.log

DDB_PATH="$PREFIX/ddb" # Or /path/to/your/ddb

exec sudo ${DDB_PATH} "$@"
