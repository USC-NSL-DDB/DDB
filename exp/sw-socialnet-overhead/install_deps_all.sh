#!/bin/bash

wget -qO- https://raw.githubusercontent.com/hjzccc/cloudlab_profile/refs/heads/main/setup.sh | bash

sudo usermod -aG docker $(whoami)
