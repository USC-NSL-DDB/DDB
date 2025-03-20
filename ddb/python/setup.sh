#!/bin/bash
SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"

VENV_PATH="$SCRIPT_DIR/env/dbg"
if [ ! -d $VENV_PATH ]; then
    python -m venv $VENV_PATH
fi

source $VENV_PATH/bin/activate
# pip3 install -r $SCRIPT_DIR/requirements.txt
python -m pip install hatch
