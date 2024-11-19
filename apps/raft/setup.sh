#!/bin/bash

sudo apt update
sudo apt install -y build-essential autoconf libtool pkg-config cmake git

# git submodule update --init
git submodule update --init --recursive

pushd libs

# pushd grpc
# git checkout v1.64.0
# popd

pushd googletest
git checkout tags/v1.15.2
popd

pushd spdlog
git checkout tags/v1.14.1
popd

popd

# Build and Install gRPC
# export MY_INSTALL_DIR=$HOME/.local
# mkdir -p $MY_INSTALL_DIR
# export PATH="$MY_INSTALL_DIR/bin:$PATH"

# cd libs/grpc
# mkdir -p cmake/build
# pushd cmake/build
# cmake -DgRPC_INSTALL=ON \
#       -DgRPC_BUILD_TESTS=OFF \
#       -DCMAKE_INSTALL_PREFIX=$MY_INSTALL_DIR \
#       ../..
# make -j$(nproc)
# make install
# popd
