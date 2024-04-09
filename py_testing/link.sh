#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
TOOL_NAME="ddb"

if ! grep -q '^#!/usr/bin/env python3$' main.py; then
  awk 'BEGIN {print "#!/usr/bin/env python3"} {print}' $SOURCE_DIR/main.py > $SOURCE_DIR/main.tmp && mv $SOURCE_DIR/main.tmp $SOURCE_DIR/main.py
fi

chmod +x $SOURCE_DIR/main.py
sudo rm -rf /usr/bin/$TOOL_NAME
sudo ln -s $SOURCE_DIR/main.py /usr/bin/$TOOL_NAME

# source .bashrc/.bash_profile/.zshrc to make the tool available in the current shell

# after that, you should be able to call the tool from the command line by "ddb"