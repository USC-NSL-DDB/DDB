#!/bin/bash

sudo apt-add-repository -y ppa:mosquitto-dev/mosquitto-ppa
sudo apt-get update
sudo apt-get install -y libc-ares-dev libssl-dev mosquitto # install mosquitto broker directly

# disable auto-start mosquitto service
sudo systemctl disable mosquitto.service
sudo systemctl stop mosquitto.service

# hack cleanup if any instance is running
sudo pkill -9 mosquitto
