#!/bin/bash

echo "Building distributed backtrace demo app"

mkdir -p build
cd build
cmake -DCMAKE_BUILD_TYPE=Debug ..
bear -- make -j$(nproc)
cd ..
