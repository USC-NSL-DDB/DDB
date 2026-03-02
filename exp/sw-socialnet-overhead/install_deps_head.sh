#!/bin/bash

go install github.com/ServiceWeaver/weaver-kube/cmd/weaver-kube@v0.23.0

git submodule update --init --recursive --jobs "$(nproc)"
