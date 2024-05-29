#!/bin/bash

# This script is used for Nu-specific image on CloudLab
# The exist pip on that image is broken.
cd /tmp
wget https://bootstrap.pypa.io/get-pip.py
python3 get-pip.py
rm -f get-pip.py

export PATH=$PATH:$HOME/.local/bin
echo "Add \$HOME/.local/bin to your path if not present yet"

# Update PyYaml and PyOpenSSL to the latest version
pip install -y pyyaml --upgrade
pip install -y pyopenssl --upgrade

