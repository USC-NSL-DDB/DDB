#!/bin/bash

# This scripts is used for end users to install DDB in one click.

sudo apt-get update
sudo apt-get install -y \
    git build-essential cmake python3 python3-pip \
    cmake make gdb

set -e
mkdir -p $HOME/.ddb_src
pushd $HOME/.ddb_src
rm -rf DDB
git clone --depth 1 --branch main https://github.com/USC-NSL-DDB/DDB.git
pushd DDB/scripts
./install.sh
./setup.sh
popd
rm -rf DDB
popd
