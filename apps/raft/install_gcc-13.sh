#!/bin/bash

sudo add-apt-repository -y ppa:ubuntu-toolchain-r/test
sudo apt-get update
sudo apt-get install -y gcc-13 g++-13

sudo update-alternatives --install /usr/bin/gcc gcc /usr/bin/gcc-13 60
sudo update-alternatives --install /usr/bin/g++ g++ /usr/bin/g++-13 60

gcc --version
