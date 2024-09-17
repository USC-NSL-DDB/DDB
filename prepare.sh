#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

sudo apt-get update
sudo apt install python3 python3-pip -y

source $SOURCE_DIR/prepare_mosquitto.sh


