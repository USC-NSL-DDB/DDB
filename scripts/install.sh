#!/bin/bash

PREFIX=${HOME}/.local

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
ROOT="$SCRIPT_DIR/.."
pushd $ROOT

echo "[Installing] DDB Core"
DDB_CORE="$ROOT/ddb/rust"
pushd $DDB_CORE
./scripts/install.sh
popd

echo "[Installing] [Deps] libfaketime"
LIBFAKETIME="$ROOT/libfaketime"
pushd $LIBFAKETIME
make -j$(nproc)
PREFIX=$PREFIX make install 
popd

echo "[Installing] [Util] DDB wrappers"
WRAPPER="$ROOT/wrapper"
pushd $WRAPPER
install -Dm0755 ddb_runapp.sh "$PREFIX/bin/ddb_runapp"
install -Dm0755 ddb_on_sudo.sh "$PREFIX/bin/ddb_on_sudo"
install -Dm0755 ddb_runapp_on_sudo.sh "$PREFIX/bin/ddb_runapp_on_sudo"
popd

