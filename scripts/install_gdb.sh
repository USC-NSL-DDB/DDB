#!/bin/bash

sudo apt-get install -y texinfo libgmp-dev libmpfr-dev flex

set -e

cd /opt
git clone https://github.com/USC-NSL-DDB/gdb-14.2.git
cd gdb-14.2
mkdir build
cd build
../configure --disable-install-man --with-python=/usr/bin/python3 && make -j$(nproc)
sudo make install
