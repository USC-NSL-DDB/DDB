#!/bin/bash

# Just offer an easier way to compile Nu app binaries

# @arg1: Nu repo path
# @arg2: command for make

# This script should be executed at the root directory of the distributed debugger repository.

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

NU_REPO=$SOURCE_DIR/$1
TARGET=$2

NCORE=$(nproc)

pushd $NU_REPO
make $TARGET -j$NCORE
popd

cp $NU_REPO/bin/* $SOURCE_DIR/nu_bin/
cp $NU_REPO/caladan/iokerneld $SOURCE_DIR/caladan_bin/
cp $NU_REPO/caladan/ksched/build/ksched.ko $SOURCE_DIR/caladan_bin/

# Iterate over each file in the source directory
# for file in "$NU_REPO/bin"/*; do
#     # Extract the filename
#     filename=$(basename "$file")
#     # Create/overwrite a symbolic link in the target directory
#     ln -sf "$file" "$SOURCE_DIR/nu_bin/$filename"
# done
