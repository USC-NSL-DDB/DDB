#!/bin/bash

echo "handle SIGUSR1 SIGUSR2 nostop noprint" >>~/.gdbinit
echo "set auto-load safe-path /" >>~/.gdbinit

# allow gdb attach via pid
echo 0 | sudo tee /proc/sys/kernel/yama/ptrace_scope

