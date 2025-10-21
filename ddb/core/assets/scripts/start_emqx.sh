#!/bin/bash

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

CONF_PATH=$SCRIPT_DIR/../conf/emqx.conf

docker stop emqx
docker rm emqx

mkdir -p /tmp/ddb/emqx/{data,logs}

docker run -d --name emqx \
  -p 10101:10101 -p 8083:8083 \
  -p 8084:8084 -p 8883:8883 \
  -p 18083:18083 \
  -v /tmp/ddb/emqx/data:/opt/emqx/data \
  -v /tmp/ddb/emqx/logs:/opt/emqx/log \
  -v $CONF_PATH:/opt/emqx/etc/emqx.conf \
  --user $(id -u):$(id -g) \
  emqx/emqx:5.8.4