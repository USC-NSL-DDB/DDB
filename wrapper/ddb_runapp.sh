#!/bin/bash

PREFIX=${HOME}/.local

CWD=$(pwd)

if [ -z "$1" ]; then
    echo "Usage: $0 <path to program> [args]"
    exit 1
fi

LIBFAKETIME="${PREFIX}/lib/faketime/libfaketime.so.1"
  
if [ ! -f "${LIBFAKETIME}" ]; then
    echo "Error: ${LIBFAKETIME} not found. Please install run 'scripts/install.sh' first."
    exit 1
fi

# export FAKETIME_NO_CACHE=1 
export FAKETIME="-00000000000000000"

program="$1"
shift
args="$@"

if [ -z "$program" ]; then
    echo "Error: No program specified."
    exit 1
fi

# if [ ! -f "$program" ]; then
#     echo "Error: Program $program not found."
#     exit 1
# fi

LD_PRELOAD="${LIBFAKETIME} ${LD_PRELOAD}" exec "${program}" ${args}
