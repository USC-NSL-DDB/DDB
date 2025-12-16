#!/bin/bash

# --- Script to set up local SSH with RSA key and authorized_keys ---

SSH_DIR="$HOME/.ssh"
PRIVATE_KEY="$SSH_DIR/id_rsa"
PUBLIC_KEY="$SSH_DIR/id_rsa.pub"
AUTHORIZED_KEYS="$SSH_DIR/authorized_keys"

echo "Starting local SSH setup..."
echo ""

if [ ! -d "$SSH_DIR" ]; then
    echo "Creating $SSH_DIR directory..."
    mkdir -p "$SSH_DIR"
else
    echo "$SSH_DIR directory already exists."
fi

chmod 700 "$SSH_DIR"
echo "Set permissions on $SSH_DIR to 700."
echo ""

if [ -f "$PUBLIC_KEY" ]; then
    echo "SSH public key already exists at $PUBLIC_KEY"
    echo "Skipping key generation."
else
    echo "No SSH key found. Generating new RSA key pair..."
    ssh-keygen -t rsa -b 4096 -N "" -f "$PRIVATE_KEY" -q
    
    if [ $? -eq 0 ]; then
        echo "SUCCESS: RSA key pair generated at $PRIVATE_KEY"
    else
        echo "FAILURE: Could not generate SSH key pair."
        exit 1
    fi
fi
echo ""

if [ ! -f "$AUTHORIZED_KEYS" ]; then
    echo "Creating $AUTHORIZED_KEYS file..."
    touch "$AUTHORIZED_KEYS"
fi

if grep -q -f "$PUBLIC_KEY" "$AUTHORIZED_KEYS" 2>/dev/null; then
    echo "Public key already present in $AUTHORIZED_KEYS"
else
    echo "Adding public key to $AUTHORIZED_KEYS..."
    cat "$PUBLIC_KEY" >> "$AUTHORIZED_KEYS"
    echo "Public key added successfully."
fi
echo ""

chmod 700 "$SSH_DIR"
chmod 600 "$PRIVATE_KEY"
chmod 644 "$PUBLIC_KEY"
chmod 600 "$AUTHORIZED_KEYS"

# Test SSH connection to localhost
echo "Testing SSH connection to localhost..."
SSH_TEST_OUTPUT=$(ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no localhost 'echo "SSH test successful"' 2>&1)
SSH_TEST_EXIT_CODE=$?

if [ $SSH_TEST_EXIT_CODE -eq 0 ]; then
    echo "SUCCESS: SSH connection to localhost works!"
    echo "Output: $SSH_TEST_OUTPUT"
else
    echo "WARNING: SSH connection test failed with exit code $SSH_TEST_EXIT_CODE"
    echo "Error output: $SSH_TEST_OUTPUT"
    echo ""
    echo "Possible issues:"
    echo "  - SSH daemon (sshd) may not be running. Check with: systemctl status sshd"
    echo "  - SSH service may not be installed. Install with: sudo apt-get install openssh-server"
    echo "  - Firewall may be blocking SSH connections"
    echo ""
    echo "The SSH keys and authorized_keys have been set up correctly."
    echo "Once sshd is running, you should be able to SSH to localhost."
fi
echo ""

echo "=========================================="
echo "SSH Setup Summary"
echo "=========================================="
echo "SSH directory: $SSH_DIR"
echo "Private key: $PRIVATE_KEY"
echo "Public key: $PUBLIC_KEY"
echo "Authorized keys: $AUTHORIZED_KEYS"
echo ""
