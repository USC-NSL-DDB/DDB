#!/bin/bash

# This scripts is used for end users to install DDB in one click.

sudo apt-get update
sudo apt-get install -y \
    git build-essential cmake python3 python3-pip \
    cmake make gdb

set -e
mkdir $HOME/.ddb_src
pushd $HOME/.ddb_src
git clone --branch main https://github.com/USC-NSL-DDB/DDB.git
cd DDB/scripts
./install.sh
./setup.sh
popd
