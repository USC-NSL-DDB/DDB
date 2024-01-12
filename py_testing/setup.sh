#!/bin/bash

VENV_PATH="./env/dbg"
python3 -m venv $VENV_PATH

source $VENV_PATH/bin/activate

pip3 install -r ./requirements.txt