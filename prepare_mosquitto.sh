#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

sudo apt update
sudo apt install -y build-essential autoconf libtool pkg-config cmake git

sudo apt-get update
sudo apt-get install -y build-essential gcc make cmake cmake-gui cmake-curses-gui 
sudo apt-add-repository -y ppa:mosquitto-dev/mosquitto-ppa
sudo apt-get update
sudo apt-get install -y libc-ares-dev libssl-dev mosquitto # install mosquitto broker directly

TMP_FOLDER="/tmp/mosquitto"

sudo apt-get update
sudo apt-get install -y build-essential gcc make cmake cmake-gui cmake-curses-gui 
sudo apt-add-repository -y ppa:mosquitto-dev/mosquitto-ppa
sudo apt-get update
sudo apt-get install -y libc-ares-dev libssl-dev mosquitto # install mosquitto broker directly

rm -rf $TMP_FOLDER
mkdir -p $TMP_FOLDER
pushd $TMP_FOLDER
git clone https://github.com/eclipse/paho.mqtt.c.git
cd paho.mqtt.c
make -j$(nproc)
sudo make uninstall # clean up first
sudo make install # install mosquitto c lib
popd

# sudo mv /etc/mosquitto/mosquitto.conf /etc/mosquitto/mosquitto.conf.bak
# pushd $SOURCE_DIR
# sudo cp ./conf/mosquitto.conf /etc/mosquitto/
# popd
