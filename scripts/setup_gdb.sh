#!/bin/bash

# Init gdb experience
sudo chmod a+rw ~/.gdbinit > /dev/null 2>&1
touch ~/.gdbinit
echo "handle SIGUSR1 SIGUSR2 nostop noprint" >> ~/.gdbinit
echo "set auto-load safe-path /" >> ~/.gdbinit


GDB_CFG_PATH="$HOME/.config/gdb"
mkdir -p $GDB_CFG_PATH
touch "$GDB_CFG_PATH/gdbinit"
echo "handle SIGUSR1 SIGUSR2 nostop noprint" >> "$GDB_CFG_PATH/gdbinit"
echo "set auto-load safe-path /" >> "$GDB_CFG_PATH/gdbinit"

# allow gdb attach via pid
echo 0 | sudo tee /proc/sys/kernel/yama/ptrace_scope
