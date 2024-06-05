#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

TMP_FOLDER="/tmp/mosquitto"

rm -rf $TMP_FOLDER
mkdir -p $TMP_FOLDER
pushd $TMP_FOLDER
git clone https://github.com/eclipse/paho.mqtt.c.git
cd paho.mqtt.c
make -j$(nproc)
sudo make install

# sudo apt-get install -y mosquitto mosquitto-clients

# sudo mv /etc/mosquitto/mosquitto.conf /etc/mosquitto/mosquitto.conf.bak
# pushd $SOURCE_DIR
# sudo cp ./conf/mosquitto.conf /etc/mosquitto/
# popd
