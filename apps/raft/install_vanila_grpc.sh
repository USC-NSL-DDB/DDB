#!/bin/bash
sudo apt update
sudo apt install -y \
      cmake \
	git \
	build-essential \
	autoconf \
      libtool \
      pkg-config

pushd libs/
git clone --recurse-submodules -b v1.64.0 --depth 1 --shallow-submodules https://github.com/grpc/grpc

# Build and Install gRPC
export MY_INSTALL_DIR=$HOME/.local
mkdir -p $MY_INSTALL_DIR
export PATH="$MY_INSTALL_DIR/bin:$PATH"

pushd grpc
mkdir -p cmake/build
pushd cmake/build
cmake -DgRPC_INSTALL=ON \
      -DgRPC_BUILD_TESTS=OFF \
      -DCMAKE_INSTALL_PREFIX=$MY_INSTALL_DIR \
      ../..
make -j$(nproc)
make install
popd
popd
popd
