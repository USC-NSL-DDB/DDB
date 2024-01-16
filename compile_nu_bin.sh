#!/bin/bash

# Just offer an easier way to compile Nu app binaries

# @arg1: Nu repo path
# @arg2: command for make

# This script should be executed at the root directory of the distributed debugger repository.

NU_REPO=$1
TARGET=$2

pushd $NU_REPO
make $TARGET
popd

cp $NU_REPO/bin/* nu_bin/
