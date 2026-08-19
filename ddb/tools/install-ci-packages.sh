#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: install-ci-packages.sh PACKAGE..." >&2
  exit 2
fi

missing_packages=()
for package in "$@"; do
  if ! dpkg-query --show --showformat='${db:Status-Status}' "${package}" 2>/dev/null |
    grep -qx installed; then
    missing_packages+=("${package}")
  fi
done

if [[ "${#missing_packages[@]}" -eq 0 ]]; then
  echo "CI system packages are already installed: $*"
  exit 0
fi

apt_options=(
  -o Acquire::Retries=3
  -o Acquire::http::Timeout=30
  -o Acquire::https::Timeout=30
  -o Dpkg::Lock::Timeout=60
)

echo "Installing missing CI system packages: ${missing_packages[*]}"
sudo env DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" update
sudo env DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" install \
  --no-install-recommends \
  --yes \
  "${missing_packages[@]}"
