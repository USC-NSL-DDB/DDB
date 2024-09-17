#!/bin/bash

# allow gdb attach via pid
echo 0 | sudo tee /proc/sys/kernel/yama/ptrace_scope

