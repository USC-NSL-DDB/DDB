#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

TMP_FOLDER="/tmp/mosquitto"

sudo apt-get update
sudo apt-add-repository -y ppa:mosquitto-dev/mosquitto-ppa
sudo apt-get update
sudo apt-get install -y build-essential gcc make cmake cmake-gui cmake-curses-gui mosquitto

rm -rf $TMP_FOLDER
mkdir -p $TMP_FOLDER
pushd $TMP_FOLDER
git clone https://github.com/eclipse/paho.mqtt.c.git
cd paho.mqtt.c
make -j$(nproc)
sudo make install


# sudo mv /etc/mosquitto/mosquitto.conf /etc/mosquitto/mosquitto.conf.bak
# pushd $SOURCE_DIR
# sudo cp ./conf/mosquitto.conf /etc/mosquitto/
# popd
