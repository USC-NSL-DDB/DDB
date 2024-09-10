#!/bin/bash

# Check if rustup is installed
if command -v rustup &> /dev/null
then
    echo "Rustup is already installed."
else
    echo "Rustup is not installed. Installing now."

    # Download and run the Rust installation script
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

    # Add cargo to the PATH
    source $HOME/.cargo/env

    echo "source $HOME/.cargo/env" >> $HOME/.bashrc

    echo "Rustup, Rust and Cargo have been installed."
fi

# Toolchains for building gdb
sudo apt-get install -y texinfo libgmp-dev libmpfr-dev flex
