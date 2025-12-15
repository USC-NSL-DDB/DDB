#!/bin/bash

# This scripts is used for end users to install DDB in one click.

mkdir $HOME/.ddb_src
pushd $HOME/.ddb_src
git clone https://github.com/USC-NSL-DDB/DDB.git
cd DDB/scripts
./install.sh
popd
