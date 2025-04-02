#!/bin/bash

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source $SCRIPT_DIR/shared.sh

rm -rf $RAFTLOGS_DIR
rm -rf $REDISLOGS_DIR
