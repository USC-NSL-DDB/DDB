#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

source $SOURCE_DIR/setup_gdb.sh
source $SOURCE_DIR/setup_mqtt.sh
source $SOURCE_DIR/setup_ptrace.sh
