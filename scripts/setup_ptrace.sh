#!/bin/bash

# --- Script to set kernel.yama.ptrace_scope permanently to 0 ---

SYSCTL_CONF_FILE="/etc/sysctl.d/10-disable-ptrace-restrictions.conf"
CONFIG_LINE="kernel.yama.ptrace_scope = 0"

echo "Setting $CONFIG_LINE for persistent configuration..."

echo "$CONFIG_LINE" | sudo tee "$SYSCTL_CONF_FILE" > /dev/null
echo "Applying new sysctl settings immediately..."
sudo sysctl --system

CURRENT_SCOPE=$(cat /proc/sys/kernel/yama/ptrace_scope)

if [ "$CURRENT_SCOPE" -eq 0 ]; then
    echo "SUCCESS: kernel.yama.ptrace_scope is now set to 0."
    echo "The setting is also saved in $SYSCTL_CONF_FILE for persistence."
else
    echo "FAILURE: The setting could not be confirmed. Current value: $CURRENT_SCOPE"
fi