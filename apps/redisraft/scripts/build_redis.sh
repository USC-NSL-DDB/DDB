#!/bin/bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source $SCRIPT_DIR/shared.sh

pushd $REDIS_SOURCE_DIR
make distclean
make -j$(nproc)
make PREFIX=$REDIS_BIN_INSTALL_DIR install
popd

echo "Redis installed to $REDIS_BIN_INSTALL_DIR"

# Check if Redis binaries are in the path
if ! command -v redis-server &> /dev/null; then
    echo "Warning: redis-server not found in PATH"
    echo "Consider adding $REDIS_BIN_INSTALL_DIR/bin to your PATH:"
    echo "export PATH=\"\$PATH:$REDIS_BIN_INSTALL_DIR/bin\""
fi
