#!/bin/bash

VERSION="0.1.1"

# Handle version flag
if [ "$1" = "--version" ] || [ "$1" = "-v" ]; then
  echo "ddb_runapp v${VERSION}"
  exit 0
fi

PREFIX=${HOME}/.local

CWD=$(pwd)

if [ -z "$1" ]; then
  echo "Usage: $0 <path to program> [args]"
  exit 1
fi

LIBFAKETIME="${PREFIX}/lib/faketime/libfaketimeMT.so.1"

if [ ! -f "${LIBFAKETIME}" ]; then
  echo "Error: ${LIBFAKETIME} not found. Please install run 'scripts/install.sh' first."
  exit 1
fi

export FAKETIME_NO_CACHE=1
export FAKETIME="-00000000000000000"

# Detect architecture and set faketime-related env vars accordingly
ARCH=$(uname -m 2>/dev/null || echo unknown)
case "$ARCH" in
aarch64 | arm64)
  # This explicitly enables monotonic clock faketime on ARM64.
  # By default, libfaketime on ARM64 does not fake the monotonic clock.
  export DONT_FAKE_MONOTONIC=0
  # echo "Detected architecture $ARCH: exporting DONT_FAKE_MONOTONIC=0"
  ;;
x86_64 | amd64 | i386 | i486 | i586 | i686 | x86)
  # x86 variants: no special faketime env needed
  ;;
*)
  echo "Warn: DDB FAKETIME has not been officially tested on this arch: ${ARCH}, PET (pause-erased time) might not be working properly."
  ;;
esac

program="$1"
shift
args=($@)

if [ -z "$program" ]; then
  echo "Error: No program specified."
  exit 1
fi

LD_PRELOAD="${LIBFAKETIME} ${LD_PRELOAD}" exec "${program}" "${args[@]}"
