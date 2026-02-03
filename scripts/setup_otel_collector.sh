#!/bin/bash

SOURCE_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
ASSETS_DIR="$SOURCE_DIR/assets"

sudo apt-get install -y wget

arch=$(uname -m)

DDB_TMP_DIR="$HOME/.ddb/tmp"
mkdir -p ${DDB_TMP_DIR}

if [[ "$arch" == "x86_64" || "$arch" == "amd64" ]]; then
    wget -O ${DDB_TMP_DIR}/otelcol.deb https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.143.1/otelcol-contrib_0.143.1_linux_amd64.deb
    :
elif [[ "$arch" == "arm64" || "$arch" == "aarch64" ]]; then
    wget -O ${DDB_TMP_DIR}/otelcol.deb https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.143.1/otelcol-contrib_0.143.1_linux_arm64.deb
    :
else
    echo "Architecture $arch is not supported."
    exit 1
fi

sudo dpkg -i ${DDB_TMP_DIR}/otelcol.deb

OTEL_CONFIG_PATH=$ASSETS_DIR/otelcol_config.yaml
sudo cp $OTEL_CONFIG_PATH /etc/otelcol-contrib/config.yaml
sudo systemctl restart otelcol-contrib.service
echo "[Setup] OpenTelemetry Collector is installed and configured."
