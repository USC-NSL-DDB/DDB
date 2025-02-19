#!/bin/bash

sudo apt-get install -y build-essential gcc make cmake cmake-gui cmake-curses-gui

mkdir -p /tmp
pushd /tmp/
git clone https://github.com/eclipse/paho.mqtt.c.git

pushd paho.mqtt.c/
make -j$(nrpoc)
sudo make install
popd

rm -rf paho.mqtt.c/
popd
