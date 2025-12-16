#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

sudo apt-get update
sudo apt-get install -y \
    build-essential gcc autoconf libtool  \
    pkg-config make cmake cmake-curses-gui git \
    python3 python3-pip libssl-dev libc-ares-dev

# sudo apt-add-repository -y ppa:mosquitto-dev/mosquitto-ppa
# sudo apt-get update
# sudo apt-get install -y libc-ares-dev libssl-dev mosquitto # install mosquitto broker directly

# # disable auto-start mosquitto service
# sudo systemctl disable mosquitto.service
# sudo systemctl stop mosquitto.service
# # hack cleanup if any instance is running
# sudo pkill -9 mosquitto

TMP_FOLDER="/tmp/mqtt"

rm -rf $TMP_FOLDER
mkdir -p $TMP_FOLDER
chmod 755 $TMP_FOLDER

set -e
git clone https://github.com/eclipse/paho.mqtt.c.git
cd paho.mqtt.c
make -j$(nproc)
set +e
sudo make uninstall # clean up first
sudo make install   # install mosquitto c lib
