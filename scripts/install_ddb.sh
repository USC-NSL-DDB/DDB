#!/bin/bash

# This scripts is used for end users to install DDB in one click.

sudo apt-get update
sudo apt-get install -y \
    git build-essential cmake python3 python3-pip \
    cmake make gdb wget unzip

set -e
mkdir -p $HOME/.ddb_src
pushd $HOME/.ddb_src

# Download VSCode extension for DDB
rm -rf *.zip || true
rm -rf *.vsix || true
wget https://github.com/USC-NSL-DDB/vscode-adapter/releases/download/v0.0.6-alpha/ddb-debugger-vsix.zip
unzip ddb-debugger-vsix.zip
rm -rf *.zip

rm -rf DDB
git clone --depth 1 --branch main https://github.com/USC-NSL-DDB/DDB.git
pushd DDB/scripts
./install.sh
./setup.sh
popd
rm -rf DDB
popd
