#!/bin/bash

set -e

CORES=$(nproc)

fw_folder="../rpc_frameworks"

# Initialize submodules
git submodule init
git submodule update --init -f --recursive

clean() {
  cd $fw_folder
  for mod in grpc Nu; do
    cd $mod
    git checkout .
    git clean -df .
    # rm -rf build/
    cd ..
  done
  cd ..
}

if [ "$1" = "clean" ]; then
  clean
  exit 0
fi

echo building grpc
rm -rf $fw_folder/grpc/include/cereal $fw_folder/grpc/include/ddb >/dev/null 2>&1
cp -r ../connector/cereal ../connector/ddb $fw_folder/grpc/include/
cd $fw_folder/grpc
if ! ./setup.sh; then
  echo "Building grpc failed"
  exit 1
fi
cd ..

echo building Nu
rm -rf $fw_folder/Nu/include/ddb >/dev/null 2>&1
cp -r ../connector/ddb $fw_folder/Nu/include/
cd $fw_folder/Nu
NODE_TYPE=c6526 ./build_all.sh
cd ../../
