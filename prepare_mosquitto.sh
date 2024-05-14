#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

sudo apt-get install -y mosquitto mosquitto-clients

# sudo mv /etc/mosquitto/mosquitto.conf /etc/mosquitto/mosquitto.conf.bak
# pushd $SOURCE_DIR
# sudo cp ./conf/mosquitto.conf /etc/mosquitto/
# popd
